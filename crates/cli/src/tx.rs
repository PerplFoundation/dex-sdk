use alloy::{primitives::TxHash, providers::Provider, sol_types::SolEventInterface};
use colored::Colorize;
use perpl_sdk::{
    abi::dex::Exchange::ExchangeEvents,
    error::{DexError, ProviderError},
    stream::RawEvent,
};

pub(crate) async fn render<P: Provider + Clone>(
    provider: P,
    tx_hash: TxHash,
) -> anyhow::Result<()> {
    let receipt = provider
        .get_transaction_receipt(tx_hash)
        .await
        .map_err(|err| DexError::Provider(err.into()))?
        .ok_or(DexError::Provider(ProviderError::InvalidRequest(
            "Transaction not found".to_string(),
        )))?;

    let mut events = Vec::with_capacity(receipt.inner.logs().len());
    for log in receipt.inner.logs() {
        events.push(RawEvent::new(
            log.transaction_hash.unwrap_or_default(),
            log.transaction_index.unwrap_or_default(),
            log.log_index.unwrap_or_default(),
            ExchangeEvents::decode_log(&log.inner)
                .map_err(|err| DexError::Provider(err.into()))?
                .data,
        ));
    }

    println!("\n{}\n", format!("**** Tx {}", tx_hash).bright_blue());

    let mut order_request = false;
    for event in events {
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
