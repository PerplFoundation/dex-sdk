use alloy::{eips::BlockId, providers::Provider, sol_types::SolEventInterface};
use colored::Colorize;
use perpl_sdk::{Chain, abi::dex::Exchange::ExchangeEvents, error::DexError, stream::RawEvent};

pub(crate) async fn render<P: Provider + Clone>(
    chain: &Chain,
    provider: P,
    block_number: u64,
) -> anyhow::Result<()> {
    let exchange = chain.exchange();
    let receipts = provider
        .get_block_receipts(BlockId::number(block_number))
        .await
        .map_err(DexError::from)?
        .ok_or(DexError::InvalidRequest("Block not found".to_string()))?;

    let mut events = Vec::with_capacity(receipts.iter().map(|r| r.inner.logs().len()).sum());
    for receipt in receipts {
        for log in receipt
            .inner
            .logs()
            .iter()
            .filter(|log| log.address() == exchange)
        {
            events.push(RawEvent::new(
                log.transaction_hash.unwrap_or_default(),
                log.transaction_index.unwrap_or_default(),
                log.log_index.unwrap_or_default(),
                ExchangeEvents::decode_log(&log.inner)
                    .map_err(DexError::from)?
                    .data,
            ));
        }
    }

    println!("\n{}\n", format!("**** Block #{}", block_number).bold().purple());

    let mut prev_tx = None;
    let mut order_request = false;

    for event in events {
        if prev_tx.is_none_or(|tx| tx < event.tx_index()) {
            println!(
                "\n{}\n",
                format!("**** Tx #{} ({})", event.tx_index(), event.tx_hash()).bright_blue()
            );
            order_request = false;
        }
        prev_tx = Some(event.tx_index());

        match event.event() {
            ExchangeEvents::OrderRequest { .. } => {
                println!("{}", format!("  {}: {:?}", event.log_index(), event.event()).cyan());
                order_request = true;
            },
            ExchangeEvents::OrderBatchCompleted { .. } => {
                println!("{}", format!("  {}: {:?}", event.log_index(), event.event()).cyan());
                order_request = false;
            },
            _ => {
                println!(
                    "{}",
                    format!(
                        "  {}{}: {:?}",
                        if order_request { "   ↳ " } else { "" },
                        event.log_index(),
                        event.event()
                    )
                    .bright_cyan()
                );
            },
        }
    }

    println!();

    Ok(())
}
