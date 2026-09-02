use std::pin::pin;

use alloy::providers::Provider;
use colored::Colorize;
use fastnum::UD64;
use futures::StreamExt;
use perpl_sdk::{
    Chain, stream,
    types::{BuilderAttribution, StateInstant},
};
use tokio_util::sync::CancellationToken;

use crate::highlight::Highlights;

/// Renders the builder attribution of a fill, empty without a builder.
fn builder_suffix(builder: Option<BuilderAttribution>, builder_fee: UD64) -> String {
    builder
        .map(|b| format!(" [builder {}: {}]", b.builder_id(), builder_fee))
        .unwrap_or_default()
}

pub(crate) async fn render<P: Provider + Clone>(
    chain: Chain,
    provider: P,
    highlights: &Highlights,
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
                let taker = format!(
                    "  Taker {} {:?} {} @ {} on perp={} (fee: {}){}",
                    trade.taker_account_id,
                    trade.taker_side,
                    trade.total_size(),
                    trade.avg_price().unwrap_or_default(),
                    trade.perpetual_id,
                    trade.taker_fee,
                    builder_suffix(trade.taker_builder, trade.taker_builder_fee),
                );
                println!("\n{}", highlights.account(trade.taker_account_id, taker));
                for fill in &trade.maker_fills {
                    let maker = format!(
                        "    <- Maker {} order {} filled {} @ {} (fee: {}){}",
                        fill.maker_account_id,
                        fill.maker_order_id,
                        fill.size,
                        fill.price,
                        fill.fee,
                        builder_suffix(fill.builder, fill.builder_fee),
                    );
                    println!("{}", highlights.account(fill.maker_account_id, maker));
                }
            }
        }

        if let Some(ref mut count) = blocks_left {
            *count -= 1;
        }
    }

    Ok(())
}
