use std::pin::pin;

use alloy::providers::Provider;
use colored::Colorize;
use futures::StreamExt;
use perpl_sdk::{Chain, abi::dex::Exchange::ExchangeEvents, state::Exchange, stream};
use tokio_util::sync::CancellationToken;

pub(crate) async fn render<P: Provider + Clone>(
    chain: Chain,
    provider: P,
    mut exchange: Exchange,
    num_blocks: Option<u64>,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    println!("{}\n", format!("{:#^144}", " Initial Snapshot ").bold().purple());
    println!("{:#}", exchange);

    let stream = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut stream = pin!(stream);

    let mut blocks_left = num_blocks;

    while let Some(res) = stream.next().await {
        if cancellation_token.is_cancelled() || blocks_left.is_some_and(|count| count == 0) {
            println!("{}\n", format!("{:#^144}", " Final Snapshot ").bold().purple());
            println!("\n{:#}\n", exchange);
            break;
        }

        let block_events = res?;
        println!("\n\n{}\n", format!("{:=^144}", " Block Events ").bold().purple());

        println!("{}", format!("{}", block_events.instant()).bold().purple());

        let state_events = exchange.apply_events(&block_events)?;

        let mut prev_tx = None;
        let state_events_ref = state_events.as_ref();
        let mut state_event_iter = state_events_ref
            .iter()
            .flat_map(|be| be.events())
            .peekable();
        let mut order_request = false;
        for block_event in block_events.events() {
            if prev_tx.is_none_or(|tx| tx < block_event.tx_index()) {
                println!(
                    "\n{}\n",
                    format!("**** Tx #{} ({})", block_event.tx_index(), block_event.tx_hash())
                        .bright_blue()
                );
                order_request = false;
            }
            prev_tx = Some(block_event.tx_index());
            match block_event.event() {
                ExchangeEvents::OrderRequest { .. } => {
                    println!(
                        "{}",
                        format!("  {}: {:?}", block_event.log_index(), block_event.event()).cyan()
                    );
                    order_request = true;
                },
                ExchangeEvents::OrderBatchCompleted { .. } => {
                    println!(
                        "{}",
                        format!("  {}: {:?}", block_event.log_index(), block_event.event()).cyan()
                    );
                    order_request = false;
                },
                _ => {
                    println!(
                        "{}",
                        format!(
                            "  {}{}: {:?}",
                            if order_request { "   ↳ " } else { "" },
                            block_event.log_index(),
                            block_event.event()
                        )
                        .bright_cyan()
                    );
                },
            }

            // State events produced from exchange events
            while let Some(state_events) = state_event_iter.peek()
                && state_events.tx_index() == block_event.tx_index()
                && state_events.log_index() == block_event.log_index()
            {
                for event in state_events.event() {
                    println!(
                        "{}",
                        format!("      {} {:?}", if order_request { "  ↳" } else { "↳" }, event)
                            .bright_green()
                    );
                }
                state_event_iter.next();
            }
        }

        // Remaining state events
        for state_events in state_event_iter.by_ref() {
            for event in state_events.event() {
                println!("{}", format!("  > {:?}", event).bright_green());
            }
        }

        if let Some(ref mut count) = blocks_left {
            *count -= 1;
        }
    }

    Ok(())
}
