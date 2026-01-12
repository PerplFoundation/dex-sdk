use std::pin::pin;

use alloy::providers::Provider;
use colored::Colorize;
use futures::StreamExt;
use perpl_sdk::{Chain, stream, types::StateInstant};
use tokio_util::sync::CancellationToken;

pub(crate) async fn render<P: Provider + Clone>(
    chain: Chain,
    provider: P,
    num_blocks: Option<u64>,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let block_num = provider.get_block_number().await?;

    let raw_events_stream =
        stream::raw(&chain, provider.clone(), StateInstant::new(block_num, 0), tokio::time::sleep);
    let trades_stream = stream::trade(&chain, provider, raw_events_stream).await?;
    let mut trades_stream = pin!(trades_stream);

    let mut blocks_left = num_blocks;

    while let Some(Ok(trades)) = trades_stream.next().await {
        if cancellation_token.is_cancelled() || blocks_left.is_some_and(|count| count == 0) {
            break;
        }

        if !trades.events().is_empty() {
            println!(
                "\n{}",
                format!("Block {} - {} trade(s):", trades.instant(), trades.events().len())
                    .bold()
                    .purple()
            );
            for event in trades.events() {
                let trade = event.event();
                println!(
                    "\n  Taker {} {:?} {} @ {} on perp={} (fee: {})",
                    trade.taker_account_id,
                    trade.taker_side,
                    trade.total_size(),
                    trade.avg_price().unwrap_or_default(),
                    trade.perpetual_id,
                    trade.taker_fee,
                );
                for fill in &trade.maker_fills {
                    println!(
                        "    <- Maker {} order {} filled {} @ {} (fee: {})",
                        fill.maker_account_id, fill.maker_order_id, fill.size, fill.price, fill.fee,
                    );
                }
            }
        }

        if let Some(ref mut count) = blocks_left {
            *count -= 1;
        }
    }

    Ok(())
}
