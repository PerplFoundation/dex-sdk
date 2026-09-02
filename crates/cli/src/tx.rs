use alloy::{primitives::TxHash, providers::Provider, sol_types::SolEventInterface};
use colored::Colorize;
use perpl_sdk::{
    Chain,
    abi::dex::Exchange::ExchangeEvents,
    error::{DexError, ProviderError},
    stream::RawEvent,
};

use crate::highlight::Highlights;

pub(crate) async fn render<P: Provider + Clone>(
    chain: &Chain,
    provider: P,
    tx_hash: TxHash,
    highlights: &Highlights,
) -> anyhow::Result<()> {
    let exchange = chain.exchange();
    let receipt = provider
        .get_transaction_receipt(tx_hash)
        .await
        .map_err(|err| DexError::Provider(err.into()))?
        .ok_or(DexError::Provider(ProviderError::InvalidRequest(
            "Transaction not found".to_string(),
        )))?;

    let mut events = Vec::with_capacity(receipt.inner.logs().len());
    // A transaction can touch contracts other than the exchange - an oracle
    // feed alongside a batch of orders, say - and those logs are none of the
    // exchange ABI's business
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
                .map_err(|err| DexError::Provider(err.into()))?
                .data,
        ));
    }

    println!("\n{}\n", format!("**** Tx {}", tx_hash).bright_blue());

    let mut order_request = false;
    for event in events {
        let line = match event.event() {
            ExchangeEvents::OrderRequest { .. } => {
                order_request = true;
                format!("  {}: {:?}", event.log_index(), event.event())
                    .cyan()
                    .to_string()
            },
            ExchangeEvents::OrderBatchCompleted { .. } => {
                order_request = false;
                format!("  {}: {:?}", event.log_index(), event.event())
                    .cyan()
                    .to_string()
            },
            _ => format!(
                "  {}{}: {:?}",
                if order_request { "   ↳ " } else { "" },
                event.log_index(),
                event.event()
            )
            .bright_cyan()
            .to_string(),
        };
        println!("{}", highlights.raw_event(event.event(), line));
    }

    println!();

    Ok(())
}
