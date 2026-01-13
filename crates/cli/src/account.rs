use std::{
    collections::{HashMap, VecDeque},
    io::Write,
    pin::pin,
};

use alloy::{primitives::TxHash, providers::Provider};
use colored::Colorize;
use crossterm::{
    QueueableCommand,
    cursor::MoveTo,
    execute,
    style::Print,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use fastnum::UD64;
use futures::StreamExt;
use perpl_sdk::{Chain, state::Exchange, stream, types};
use tabled::{Table, Tabled, settings::Style};
use tokio_util::sync::CancellationToken;

pub(crate) async fn render<P: Provider + Clone>(
    chain: Chain,
    provider: P,
    mut exchange: Exchange,
    num_blocks: Option<u64>,
    num_trades: usize,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let stream = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut stream = pin!(stream);

    let mut blocks_left = num_blocks;

    let mut last_trades: HashMap<_, _> = exchange
        .perpetuals()
        .keys()
        .map(|perp_id| (*perp_id, VecDeque::<TradeDetails>::new()))
        .collect();

    let mut stdout = std::io::stdout();

    execute!(stdout, EnterAlternateScreen, Clear(ClearType::All), MoveTo(0, 0))?;

    while let Some(res) = stream.next().await {
        if cancellation_token.is_cancelled() || blocks_left.is_some_and(|count| count == 0) {
            break;
        }

        let block_events = res?;
        let state_events = exchange.apply_events(&block_events)?;

        let account = exchange.accounts().values().last().unwrap();
        let mut perpetuals: Vec<_> = exchange.perpetuals().values().collect();
        perpetuals.sort_by_key(|p| p.id());

        state_events.iter().for_each(|block_events| {
            block_events.events().iter().for_each(|events| {
                events.event().iter().for_each(|event| {
                    if let Some(trade) = event.as_trade()
                        && let Some(perp_trades) = last_trades.get_mut(&trade.perpetual_id)
                    {
                        if trade.taker_account_id == account.id() {
                            perp_trades.push_back(TradeDetails {
                                block: block_events.instant().block_number(),
                                tx_hash: events.tx_hash(),
                                liquidity_side: "Taker".yellow().to_string(),
                                side: if trade.taker_side == types::OrderSide::Ask {
                                    trade.taker_side.to_string().red().to_string()
                                } else {
                                    trade.taker_side.to_string().green().to_string()
                                },
                                price: if trade.taker_side == types::OrderSide::Ask {
                                    trade.avg_price().unwrap().to_string().red().to_string()
                                } else {
                                    trade.avg_price().unwrap().to_string().green().to_string()
                                },
                                size: if trade.taker_side == types::OrderSide::Ask {
                                    trade.total_size().to_string().red().to_string()
                                } else {
                                    trade.total_size().to_string().green().to_string()
                                },
                                fees: trade.taker_fee,
                            })
                        } else if let Some((avg_price, size, fees)) =
                            trade.maker_total(account.id())
                        {
                            let maker_side = trade.taker_side.opposite();
                            perp_trades.push_back(TradeDetails {
                                block: block_events.instant().block_number(),
                                tx_hash: events.tx_hash(),
                                liquidity_side: "Maker".blue().to_string(),
                                side: if maker_side == types::OrderSide::Ask {
                                    maker_side.to_string().red().to_string()
                                } else {
                                    maker_side.to_string().green().to_string()
                                },
                                price: if maker_side == types::OrderSide::Ask {
                                    avg_price.to_string().red().to_string()
                                } else {
                                    avg_price.to_string().green().to_string()
                                },
                                size: if maker_side == types::OrderSide::Ask {
                                    size.to_string().red().to_string()
                                } else {
                                    size.to_string().green().to_string()
                                },
                                fees,
                            })
                        };
                        while perp_trades.len() > num_trades {
                            perp_trades.pop_front();
                        }
                    }
                });
            })
        });

        stdout.queue(Clear(ClearType::All))?;
        stdout.queue(MoveTo(0, 0))?;

        // Exchange summary
        stdout.queue(Print(format!("{}", exchange)))?;

        // Account with positions
        stdout.queue(Print(format!("{:#}", account)))?;

        // Account orders for all perpetuals
        for perp in perpetuals {
            // Perp summary
            stdout.queue(Print("\n\n"))?;
            stdout.queue(Print(perp))?;

            // Account orders
            let ask_orders = perp
                .l3_book()
                .ask_orders()
                .filter(|o| o.account_id() == account.id())
                .collect::<Vec<_>>();
            let mut table = Table::new(
                ask_orders.iter().rev().map(|o| &*(**o)).chain(
                    perp.l3_book()
                        .bid_orders()
                        .filter(|o| o.account_id() == account.id())
                        .map(|o| &*(*o)),
                ),
            );
            table.with(Style::sharp());
            if table.count_rows() > 1 {
                stdout.queue(Print(table))?;
            }

            // Recent trades
            if let Some(trades) = last_trades.get(&perp.id())
                && !trades.is_empty()
            {
                stdout.queue(Print("\n"))?;
                let mut table = Table::new(trades);
                table.with(Style::sharp());
                stdout.queue(Print(table))?;
            }
        }

        stdout.flush()?;

        if let Some(ref mut count) = blocks_left {
            *count -= 1;
        }
    }

    execute!(stdout, LeaveAlternateScreen)?;

    Ok(())
}

#[derive(Tabled)]
struct TradeDetails {
    #[tabled(rename = "Block")]
    block: u64,
    #[tabled(rename = "Tx Hash")]
    tx_hash: TxHash,
    #[tabled(rename = "Liquidity Side")]
    liquidity_side: String,
    #[tabled(rename = "Order Side")]
    side: String,
    #[tabled(rename = "Price")]
    price: String,
    #[tabled(rename = "Size")]
    size: String,
    #[tabled(rename = "Fees")]
    fees: UD64,
}
