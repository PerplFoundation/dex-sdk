//! Market maker analysis: how a set of named accounts is distributed across a
//! perpetual order book, and how they quote and trade while it is watched.

use std::{collections::HashSet, io::Write, pin::pin};

use alloy::providers::Provider;
use colored::Colorize;
use crossterm::{
    QueueableCommand,
    cursor::MoveTo,
    execute,
    style::Print,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use fastnum::{UD64, UD128};
use futures::StreamExt;
use perpl_sdk::{
    Chain,
    state::{Exchange, OrderBook, OrderEventType, Perpetual, StateEvents},
    stream, types,
};
use tabled::{
    Table,
    settings::{Alignment, Panel, Style, object::Rows},
};
use tokio_util::sync::CancellationToken;

use crate::{args::BookArgs, highlight::Highlights};

/// Tight band around the mid the makers' resting size is measured in, the depth
/// a taker actually pays for.
const NEAR_BPS: u32 = 10;

/// Wider band around the mid, covering the size a maker keeps behind its top
/// of book.
const FAR_BPS: u32 = 50;

/// A market maker being tracked, resolved to the account it quotes from.
#[derive(Clone, Debug)]
pub(crate) struct Maker {
    pub(crate) account_id: types::AccountId,
    pub(crate) label: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn render<P: Provider + Clone>(
    chain: Chain,
    provider: P,
    mut exchange: Exchange,
    makers: Vec<Maker>,
    highlights: Highlights,
    book_args: &BookArgs,
    num_blocks: Option<u64>,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let stream = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut stream = pin!(stream);

    let mut blocks_left = num_blocks;
    let mut blocks_seen = 0u64;
    let mut activity = vec![Activity::default(); makers.len()];

    // `show mms` runs against a single perpetual, enforced while the snapshot
    // is configured. Its ID is held on to rather than re-read every block, so
    // a contract listed mid-stream cannot displace the one being watched - and
    // so the makers' activity can be scoped to it: they may well be quoting
    // other contracts at the same time, and none of that belongs in this book's
    // numbers.
    let perpetual_id = exchange
        .perpetuals()
        .values()
        .last()
        .expect("`show mms` requires one perpetual")
        .id();

    let mut stdout = std::io::stdout();

    execute!(stdout, EnterAlternateScreen, Clear(ClearType::All), MoveTo(0, 0))?;

    while let Some(res) = stream.next().await {
        if cancellation_token.is_cancelled() || blocks_left.is_some_and(|count| count == 0) {
            break;
        }

        let block_events = res?;
        let state_events = exchange.apply_events(&block_events)?;
        blocks_seen += 1;

        for block in state_events.iter() {
            for context in block.events() {
                record_activity(context.event(), perpetual_id, &makers, &mut activity);
            }
        }

        let perpetual = exchange
            .perpetuals()
            .get(&perpetual_id)
            .expect("watched perpetual stays listed");
        let footprint = Footprint::of(perpetual, &makers);

        stdout.queue(Clear(ClearType::All))?;
        stdout.queue(MoveTo(0, 0))?;

        stdout.queue(Print(format!("{}", exchange)))?;
        stdout.queue(Print(format!("{}", perpetual)))?;
        stdout.queue(Print("\n\n"))?;
        stdout.queue(Print(legend(&makers, &highlights)))?;
        stdout.queue(Print("\n"))?;
        stdout.queue(Print(distribution_table(&makers, &highlights, &footprint)))?;
        stdout.queue(Print("\n"))?;
        stdout.queue(Print(activity_table(&makers, &highlights, &activity, blocks_seen)))?;
        stdout.queue(Print("\n"))?;

        let book_view = perpetual
            .l3_book()
            .view(book_args.depth(), book_args.orders_per_level(), book_args.show_expired)
            .highlighted_by(&highlights);
        stdout.queue(Print(format!("{:#}", book_view)))?;

        stdout.flush()?;

        if let Some(ref mut count) = blocks_left {
            *count -= 1;
        }
    }

    execute!(stdout, LeaveAlternateScreen)?;

    Ok(())
}

/// What a maker has done while the book was watched, as opposed to what it is
/// resting on the book right now.
#[derive(Clone, Debug, Default)]
struct Activity {
    maker_fills: u64,
    maker_size: UD64,
    maker_notional: UD128,
    maker_fees: UD64,
    taker_fills: u64,
    taker_size: UD64,
    taker_notional: UD128,
    taker_fees: UD64,
    placed: u64,
    amended: u64,
    cancelled: u64,
}

/// Folds one event context, the state events a single exchange event produced,
/// into the makers' running activity - ignoring everything that happened on a
/// perpetual other than `perpetual_id`.
fn record_activity(
    events: &[StateEvents],
    perpetual_id: types::PerpetualId,
    makers: &[Maker],
    activity: &mut [Activity],
) {
    // A maker order that fills is reported twice within the context: once as
    // `Filled`, and once as the `Updated`/`Removed` that takes the filled size
    // off the book. Collecting the filled orders first keeps those from being
    // counted as quote amendments and cancellations.
    let filled: HashSet<_> = events
        .iter()
        .filter_map(|event| event.as_order_event())
        .filter(|event| event.perpetual_id == perpetual_id)
        .filter(|event| matches!(event.r#type, OrderEventType::Filled { is_maker: true, .. }))
        .filter_map(|event| event.order_id)
        .collect();

    for event in events {
        match event {
            StateEvents::Order(order) if order.perpetual_id == perpetual_id => {
                let Some(idx) = index_of(makers, order.account_id) else {
                    continue;
                };
                if order.order_id.is_some_and(|id| filled.contains(&id)) {
                    continue;
                }
                match order.r#type {
                    OrderEventType::Placed { .. } => activity[idx].placed += 1,
                    OrderEventType::Updated { .. } => activity[idx].amended += 1,
                    OrderEventType::Removed => activity[idx].cancelled += 1,
                    OrderEventType::Filled { .. } => {},
                }
            },
            StateEvents::Trade(trade) if trade.perpetual_id == perpetual_id => {
                if let Some(idx) = index_of(makers, trade.taker_account_id) {
                    activity[idx].taker_fills += 1;
                    activity[idx].taker_size += trade.total_size();
                    // The taker traded against every maker the trade matched,
                    // so its notional is the sum over all of them - while its
                    // fee is reported once, for the taker order as a whole
                    activity[idx].taker_notional += trade
                        .maker_fills
                        .iter()
                        .map(|fill| notional(fill.price, fill.size))
                        .sum::<UD128>();
                    activity[idx].taker_fees += trade.taker_fee;
                }
                for fill in &trade.maker_fills {
                    let Some(idx) = index_of(makers, fill.maker_account_id) else {
                        continue;
                    };
                    activity[idx].maker_fills += 1;
                    activity[idx].maker_size += fill.size;
                    activity[idx].maker_notional += notional(fill.price, fill.size);
                    activity[idx].maker_fees += fill.fee;
                }
            },
            _ => {},
        }
    }
}

fn index_of(makers: &[Maker], account_id: types::AccountId) -> Option<usize> {
    makers.iter().position(|m| m.account_id == account_id)
}

/// Resting quotes of one participant on one side of the book.
#[derive(Clone, Debug, Default)]
struct SideQuotes {
    orders: usize,
    levels: usize,
    size: UD64,
    notional: UD128,
    /// Quoted price closest to the mid.
    best: Option<UD64>,
    /// Quoted price furthest from the mid.
    worst: Option<UD64>,
    /// Size resting within [`NEAR_BPS`] of the mid.
    near: UD64,
    /// Size resting within [`FAR_BPS`] of the mid.
    far: UD64,
    /// Price of the level last folded in, to count levels while walking orders
    /// in price order.
    last_price: Option<UD64>,
}

impl SideQuotes {
    fn add(&mut self, price: UD64, size: UD64, mid: UD64) {
        if self.last_price != Some(price) {
            self.levels += 1;
            self.last_price = Some(price);
        }
        self.orders += 1;
        self.size += size;
        self.notional += notional(price, size);
        // Orders arrive in price-time priority, walking away from the spread
        self.best.get_or_insert(price);
        self.worst = Some(price);
        let bps = bps_from(mid, price);
        if bps <= UD64::from(NEAR_BPS) {
            self.near += size;
        }
        if bps <= UD64::from(FAR_BPS) {
            self.far += size;
        }
    }
}

/// Both sides of one participant's resting quotes.
#[derive(Clone, Debug, Default)]
struct Quotes {
    bids: SideQuotes,
    asks: SideQuotes,
}

/// The tracked makers' share of a book, alongside everyone else's and the
/// book's own totals.
struct Footprint {
    /// One entry per tracked maker, in the order they were given.
    makers: Vec<Quotes>,
    /// Everything quoted by accounts that are not tracked.
    others: Quotes,
    /// The whole book, tracked makers included.
    book: Quotes,
    /// Mid price the distances are measured from.
    mid: UD64,
}

impl Footprint {
    /// Walks the live orders of `perpetual`'s book once, attributing each to
    /// the maker that placed it. Expired orders are left out: they are no
    /// longer liquidity, and the book's own level sizes already exclude them.
    fn of(perpetual: &Perpetual, makers: &[Maker]) -> Self {
        let book = perpetual.l3_book();
        let mid = mid_price(book, perpetual.mark_price());
        let mut footprint = Self {
            makers: vec![Quotes::default(); makers.len()],
            others: Quotes::default(),
            book: Quotes::default(),
            mid,
        };

        for order in book.bid_orders().filter(|order| !order.is_expired()) {
            let quotes = match index_of(makers, order.account_id()) {
                Some(idx) => &mut footprint.makers[idx],
                None => &mut footprint.others,
            };
            quotes.bids.add(order.price(), order.size(), mid);
            footprint.book.bids.add(order.price(), order.size(), mid);
        }
        for order in book.ask_orders().filter(|order| !order.is_expired()) {
            let quotes = match index_of(makers, order.account_id()) {
                Some(idx) => &mut footprint.makers[idx],
                None => &mut footprint.others,
            };
            quotes.asks.add(order.price(), order.size(), mid);
            footprint.book.asks.add(order.price(), order.size(), mid);
        }

        footprint
    }
}

/// Mid of the book, falling back to the mark price when only one side - or
/// neither - is quoted.
fn mid_price(book: &OrderBook, mark_price: UD64) -> UD64 {
    match (book.best_bid(), book.best_ask()) {
        (Some((bid, _)), Some((ask, _))) => (bid + ask) / UD64::from(2u32),
        (Some((bid, _)), None) => bid,
        (None, Some((ask, _))) => ask,
        (None, None) => mark_price,
    }
}

/// Distance of `price` from `mid` in basis points. Zero on the far side of the
/// mid, which only a crossed book can produce.
fn bps_from(mid: UD64, price: UD64) -> UD64 {
    if mid.is_zero() {
        return UD64::ZERO;
    }
    let distance = if price > mid { price - mid } else { mid - price };
    distance * UD64::from(10_000u32) / mid
}

fn notional(price: UD64, size: UD64) -> UD128 { price.resize() * size.resize() }

/// Colour key for every highlighted account - the tracked makers, and the one
/// `--highlight` adds - so the painted orders in the book below can be read
/// back to an account.
fn legend(makers: &[Maker], highlights: &Highlights) -> String {
    let keys = highlights
        .entries()
        .map(|(account_id, paint)| {
            match makers.iter().find(|maker| maker.account_id == account_id) {
                Some(maker) => paint.apply(&format!(" {} ", maker.label)),
                None => paint.apply(&format!(" --highlight #{} ", account_id)),
            }
        })
        .collect::<Vec<_>>()
        .join("  ");
    format!("{} {}\n", "Makers:".bold(), keys)
}

/// One row per tracked maker describing what it is resting on the book right
/// now, closed by the untracked remainder and the book's own totals.
fn distribution_table(makers: &[Maker], highlights: &Highlights, footprint: &Footprint) -> String {
    let mut rows = vec![header(&[
        "Market Maker",
        "Account",
        "Bids n/lvl",
        "Bid Size (%)",
        "Asks n/lvl",
        "Ask Size (%)",
        "Notional",
        "Best Bid / Ask",
        "Spread bps",
        "Depth bps B/A",
        &format!("≤{}bps (%)", NEAR_BPS),
        &format!("≤{}bps (%)", FAR_BPS),
        "Skew",
    ])];

    for (maker, quotes) in makers.iter().zip(&footprint.makers) {
        rows.push(quote_row(
            highlights.account(maker.account_id, format!(" {} ", maker.label)),
            format!("#{}", maker.account_id),
            quotes,
            footprint,
        ));
    }
    rows.push(quote_row(
        "Others".dimmed().to_string(),
        "-".to_string(),
        &footprint.others,
        footprint,
    ));
    rows.push(quote_row("Book".bold().to_string(), "-".to_string(), &footprint.book, footprint));

    let mut table = Table::from_iter(rows.iter());
    table.with(Panel::header(format!(
        "Resting quotes :: mid {} :: bid size {} :: ask size {}",
        footprint.mid, footprint.book.bids.size, footprint.book.asks.size,
    )));
    table.modify(Rows::first(), Alignment::right());
    table.with(Style::modern());
    table.to_string()
}

fn quote_row(
    label: String,
    account: String,
    quotes: &Quotes,
    footprint: &Footprint,
) -> Vec<String> {
    let spread = quotes
        .bids
        .best
        .zip(quotes.asks.best)
        .filter(|_| !footprint.mid.is_zero())
        .map(|(bid, ask)| {
            if ask > bid {
                format!("{:.1}", (ask - bid) * UD64::from(10_000u32) / footprint.mid)
            } else {
                "0.0".to_string()
            }
        })
        .unwrap_or_else(|| "-".to_string());

    vec![
        label,
        account,
        format!("{} / {}", quotes.bids.orders, quotes.bids.levels),
        sized_share(quotes.bids.size, footprint.book.bids.size),
        format!("{} / {}", quotes.asks.orders, quotes.asks.levels),
        sized_share(quotes.asks.size, footprint.book.asks.size),
        format!("{:.0}", quotes.bids.notional + quotes.asks.notional),
        format!("{} / {}", price_or_dash(quotes.bids.best), price_or_dash(quotes.asks.best)),
        spread,
        format!(
            "{} / {}",
            depth_bps(footprint.mid, quotes.bids.worst),
            depth_bps(footprint.mid, quotes.asks.worst),
        ),
        // Both sides combined, against all the liquidity the book keeps in the
        // same band - the share of the depth a taker reaches that is this
        // maker's
        sized_share(
            quotes.bids.near + quotes.asks.near,
            footprint.book.bids.near + footprint.book.asks.near,
        ),
        sized_share(
            quotes.bids.far + quotes.asks.far,
            footprint.book.bids.far + footprint.book.asks.far,
        ),
        skew(quotes.bids.size, quotes.asks.size),
    ]
}

/// One row per tracked maker describing what it has done since the command
/// started watching the book.
fn activity_table(
    makers: &[Maker],
    highlights: &Highlights,
    activity: &[Activity],
    blocks_seen: u64,
) -> String {
    let mut rows = vec![header(&[
        "Market Maker",
        "Maker Fills",
        "Maker Size",
        "Maker Notional",
        "Maker Fees",
        "Taker Fills",
        "Taker Size",
        "Taker Notional",
        "Taker Fees",
        "Placed",
        "Amended",
        "Cancelled",
        "Fills / Quote",
    ])];

    for (maker, seen) in makers.iter().zip(activity) {
        rows.push(vec![
            highlights.account(maker.account_id, format!(" {} ", maker.label)),
            seen.maker_fills.to_string(),
            seen.maker_size.to_string(),
            format!("{:.0}", seen.maker_notional),
            seen.maker_fees.to_string(),
            seen.taker_fills.to_string(),
            seen.taker_size.to_string(),
            format!("{:.0}", seen.taker_notional),
            seen.taker_fees.to_string(),
            seen.placed.to_string(),
            seen.amended.to_string(),
            seen.cancelled.to_string(),
            ratio(seen.maker_fills, seen.placed),
        ]);
    }

    let mut table = Table::from_iter(rows.iter());
    table.with(Panel::header(format!(
        "Activity over {} block(s) watched :: fills and quotes counted from the event stream",
        blocks_seen,
    )));
    table.modify(Rows::first(), Alignment::right());
    table.with(Style::modern());
    table.to_string()
}

fn header(columns: &[&str]) -> Vec<String> { columns.iter().map(|c| c.to_string()).collect() }

/// A size with its share of the book's own size on that side.
fn sized_share(size: UD64, total: UD64) -> String {
    if total.is_zero() {
        return size.to_string();
    }
    format!("{} ({:.1}%)", size, size * UD64::from(100u32) / total)
}

fn price_or_dash(price: Option<UD64>) -> String {
    price.map(|p| p.to_string()).unwrap_or("-".to_string())
}

/// How far from the mid the furthest quote on a side rests.
fn depth_bps(mid: UD64, worst: Option<UD64>) -> String {
    worst
        .map(|price| format!("{:.1}", bps_from(mid, price)))
        .unwrap_or("-".to_string())
}

/// Imbalance between the two sides, positive when the bid side is heavier.
fn skew(bids: UD64, asks: UD64) -> String {
    let total = bids + asks;
    if total.is_zero() {
        return "-".to_string();
    }
    let (sign, delta) = if bids >= asks { ("+", bids - asks) } else { ("-", asks - bids) };
    format!("{}{:.1}%", sign, delta * UD64::from(100u32) / total)
}

fn ratio(numerator: u64, denominator: u64) -> String {
    if denominator == 0 {
        return "-".to_string();
    }
    format!("{:.2}", numerator as f64 / denominator as f64)
}
