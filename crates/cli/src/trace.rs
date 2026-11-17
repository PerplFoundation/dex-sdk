use std::pin::pin;

use alloy::providers::Provider;
use colored::Colorize;
use futures::StreamExt;
use perpl_sdk::{Chain, abi::dex::Exchange::ExchangeEvents, state::Exchange};
use tokio_util::sync::CancellationToken;

use crate::{Streaming, highlight::Highlights};

pub(crate) async fn render<P: Provider + Clone + 'static>(
    chain: Chain,
    provider: P,
    streaming: &Streaming,
    mut exchange: Exchange,
    highlights: &Highlights,
    num_blocks: Option<u64>,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    println!("{}\n", format!("{:#^144}", " Initial Snapshot ").bold().purple());
    println!("{:#}", exchange);

    // Every commit state at or above the minimum is traced, each block
    // again at each state; the snapshot commits at `trace_commit`, lower
    // states show as speculative state events
    let commit = streaming.trace_commit();
    let (stream, _handle) = streaming.source(&chain, provider, exchange.instant().next());
    let mut stream = pin!(stream);

    let mut blocks_left = num_blocks;

    while let Some(res) = stream.next().await {
        if cancellation_token.is_cancelled() || blocks_left.is_some_and(|count| count == 0) {
            println!("{}\n", format!("{:#^144}", " Final Snapshot ").bold().purple());
            println!("\n{:#}\n", exchange);
            break;
        }

        let block_events = res?;
        let tag = format!(
            " [{}]{}",
            block_events.commit_state(),
            if block_events.supersedes() { " [supersedes]" } else { "" }
        );
        let state_events = exchange.apply_events_at(&block_events, commit)?;
        if state_events.is_none() && !block_events.supersedes() {
            // Re-send of a block already committed: just note the progress
            println!("{}", format!("{}{}", block_events.instant(), tag).bold().purple());
            continue;
        }
        println!("\n\n{}\n", format!("{:=^144}", " Block Events ").bold().purple());

        println!("{}", format!("{}{}", block_events.instant(), tag).bold().purple());

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
            let line = match block_event.event() {
                ExchangeEvents::OrderRequest { .. } => {
                    order_request = true;
                    format!("  {}: {:?}", block_event.log_index(), block_event.event())
                        .cyan()
                        .to_string()
                },
                ExchangeEvents::OrderBatchCompleted { .. } => {
                    order_request = false;
                    format!("  {}: {:?}", block_event.log_index(), block_event.event())
                        .cyan()
                        .to_string()
                },
                _ => format!(
                    "  {}{}: {:?}",
                    if order_request { "   ↳ " } else { "" },
                    block_event.log_index(),
                    block_event.event()
                )
                .bright_cyan()
                .to_string(),
            };
            println!("{}", highlights.raw_event(block_event.event(), line));

            // State events produced from exchange events
            while let Some(state_events) = state_event_iter.peek()
                && state_events.tx_index() == block_event.tx_index()
                && state_events.log_index() == block_event.log_index()
            {
                for event in state_events.event() {
                    let line =
                        format!("      {} {:?}", if order_request { "  ↳" } else { "↳" }, event)
                            .bright_green()
                            .to_string();
                    println!("{}", highlights.state_event(event, line));
                }
                state_event_iter.next();
            }
        }

        // Remaining state events
        for state_events in state_event_iter.by_ref() {
            for event in state_events.event() {
                let line = format!("  > {:?}", event).bright_green().to_string();
                println!("{}", highlights.state_event(event, line));
            }
        }

        if let Some(ref mut count) = blocks_left {
            *count -= 1;
        }
    }

    Ok(())
}
