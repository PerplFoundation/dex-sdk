use std::{iter, ops::Deref};

use colored::Colorize;
use fastnum::UD64;
use tabled::{
    Table,
    settings::{
        Alignment, Panel, Style, Width,
        object::{Row, Rows},
    },
};

use super::{BookLevel, OrderBook};

/// View of an order book.
/// Can be rendered as plain table or compact L3 representation limited by depth
/// and number of orders per level.
pub struct OrderBookView<'a> {
    book: &'a OrderBook,
    depth: Option<usize>,
    orders_per_level: Option<usize>,
    show_expired: bool,
}

impl<'a> OrderBookView<'a> {
    pub(crate) fn new(
        book: &'a OrderBook,
        depth: Option<usize>,
        orders_per_level: Option<usize>,
        show_expired: bool,
    ) -> Self {
        Self { book, depth, orders_per_level, show_expired }
    }
}

impl<'a> std::fmt::Display for OrderBookView<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let spread_panel = |table: &mut Table, num_ask_rows: usize| {
            if let Some(((best_ask, _), (best_bid, _))) =
                self.book.best_ask().zip(self.book.best_bid())
            {
                table.with(Panel::horizontal(
                    num_ask_rows + 1,
                    format!(
                        "Best ASK: {} :: Best BID: {} :: Spread: {} ({:.2} %)",
                        best_ask,
                        best_bid,
                        best_ask - best_bid,
                        (best_ask - best_bid) / (best_ask + best_bid / 2) * 100
                    ),
                ));
                table.modify(Row::from(num_ask_rows + 1), Alignment::right());
            }
        };

        if f.alternate() {
            // Compact representation with only non-expired orders as an alternate
            // representation

            let level_orders = |level: &BookLevel| {
                let mut level_orders = String::with_capacity(64 * level.num_orders() as usize); // Guesstimate
                for (i, order) in self
                    .book
                    .level_orders(level)
                    .filter(|o| !o.is_expired() || self.show_expired)
                    .take(self.orders_per_level.unwrap_or(level.num_orders() as usize))
                    .enumerate()
                {
                    if i > 0 && i % 4 == 0 {
                        level_orders.push('\n');
                    }
                    if !order.is_expired() {
                        level_orders.push_str(format!("{:#} ", order.deref()).as_str());
                    } else {
                        level_orders.push_str(
                            format!("{:#} ", order.deref())
                                .strikethrough()
                                .to_string()
                                .as_str(),
                        );
                    }
                }

                level_orders
            };

            // Asks
            let mut asks = Vec::with_capacity(self.book.asks.len());
            let mut num_ask_levels = 0;
            let mut num_ask_orders = 0;
            let mut cumulative_ask_size = UD64::ZERO;
            for (price, level) in self
                .book
                .asks
                .iter()
                .filter(|(_, lvl)| lvl.num_orders() > 0)
            {
                num_ask_levels += 1;
                num_ask_orders += level.num_orders();
                cumulative_ask_size += level.size();
                asks.push(vec![
                    price.to_string().red().to_string(),
                    level.size().to_string().red().to_string(),
                    cumulative_ask_size.to_string().red().to_string(),
                    level.num_orders().to_string().red().to_string(),
                    level_orders(level),
                ]);
            }

            // Bids
            let mut bids = Vec::with_capacity(self.book.bids.len());
            let mut num_bid_levels = 0;
            let mut num_bid_orders = 0;
            let mut cumulative_bid_size = UD64::ZERO;
            for (price, level) in self
                .book
                .bids
                .iter()
                .filter(|(_, lvl)| lvl.num_orders() > 0)
            {
                num_bid_levels += 1;
                num_bid_orders += level.num_orders();
                cumulative_bid_size += level.size();
                bids.push(vec![
                    price.0.to_string().green().to_string(),
                    level.size().to_string().green().to_string(),
                    cumulative_bid_size.to_string().green().to_string(),
                    level.num_orders().to_string().green().to_string(),
                    level_orders(level),
                ]);
            }

            // Table of price levels
            let mut table = Table::from_iter(
                iter::once(&vec![
                    "Price".to_string(),
                    "Size".to_string(),
                    "Cum Size".to_string(),
                    "Num Orders".to_string(),
                    "Orders".to_string(),
                ])
                .chain(
                    asks.iter()
                        .take(self.depth.unwrap_or(num_ask_levels))
                        .rev()
                        .chain(bids.iter().take(self.depth.unwrap_or(num_bid_levels))),
                ),
            );

            // Header with totals
            let (ask_pct, bid_pct) =
                if cumulative_ask_size > UD64::ZERO || cumulative_bid_size > UD64::ZERO {
                    let total = cumulative_ask_size + cumulative_bid_size;
                    let ask_pct = (cumulative_ask_size / total) * UD64::from(100u32);
                    let bid_pct = (cumulative_bid_size / total) * UD64::from(100u32);
                    (ask_pct, bid_pct)
                } else {
                    (UD64::ZERO, UD64::ZERO)
                };
            table.with(Panel::header(format!(
                "Total orders: {} :: ASK orders: {}, levels: {}, size: {} ({:.1}%) :: BID orders: \
                 {}, levels: {}, size: {} ({:.1}%)",
                num_ask_orders + num_bid_orders,
                num_ask_orders,
                num_ask_levels,
                cumulative_ask_size,
                ask_pct,
                num_bid_orders,
                num_bid_levels,
                cumulative_bid_size,
                bid_pct,
            )));
            table.modify(Rows::first(), Alignment::right());

            // Spread
            spread_panel(&mut table, self.depth.unwrap_or(num_ask_levels) + 1); // +1 for header

            if let Some(max_width) = f.width() {
                table.with(Width::wrap(max_width));
            }

            table.with(Style::modern());
            writeln!(f, "{}", table)
        } else {
            // Plain table with full details by default
            let ask_orders = self.book.ask_orders().collect::<Vec<_>>();
            let num_ask_orders = ask_orders.len();
            let mut table = Table::new(
                ask_orders
                    .iter()
                    .rev()
                    .map(|o| (*o).deref())
                    .chain(self.book.bid_orders().map(|o| (*o).deref())),
            );
            spread_panel(&mut table, num_ask_orders);
            table.with(Style::sharp());
            writeln!(f, "{}", table)
        }
    }
}
