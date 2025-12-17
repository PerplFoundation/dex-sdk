//! Fill listener implementation.

use std::{collections::HashMap, future::Future, time::Duration};

use alloy::{primitives::U256, providers::Provider};
use futures::StreamExt;
use tokio::sync::mpsc;

use super::types::{BlockTrades, MakerFill, TakerTrade, TradeReceiver};
use crate::{
    Chain,
    abi::dex::Exchange::{ExchangeEvents, ExchangeInstance, MakerOrderFilled},
    error::DexError,
    num, stream,
    types::{self, OrderSide, RequestType},
};

/// Default channel buffer size.
const DEFAULT_CHANNEL_SIZE: usize = 100;

/// Configuration for normalization.
#[derive(Clone)]
pub struct NormalizationConfig {
    collateral_converter: num::Converter,
    perpetuals: HashMap<types::PerpetualId, PerpetualConverters>,
}

/// Converters for a single perpetual.
#[derive(Clone, Copy)]
struct PerpetualConverters {
    price_converter: num::Converter,
    size_converter: num::Converter,
}

/// Context for tracking order requests (reuses pattern from exchange.rs).
struct OrderContext {
    account_id: types::AccountId,
    side: OrderSide,
}

/// Pending maker fill waiting for taker match.
struct PendingMakerFill {
    tx_hash: alloy::primitives::TxHash,
    log_index: u64,
    perpetual_id: types::PerpetualId,
    maker_account_id: types::AccountId,
    maker_order_id: types::OrderId,
    price: fastnum::UD64,
    size: fastnum::UD64,
    maker_fee: fastnum::UD64,
}

/// Trade processor - pure logic, no async.
pub struct TradeProcessor {
    config: NormalizationConfig,
    order_context: Option<OrderContext>,
    pending_maker_fills: Vec<PendingMakerFill>,
    prev_tx_index: Option<u64>,
}

impl TradeProcessor {
    /// Create a new trade processor with the given normalization config.
    pub fn new(config: NormalizationConfig) -> Self {
        Self {
            config,
            order_context: None,
            pending_maker_fills: Vec::new(),
            prev_tx_index: None,
        }
    }

    /// Process a block of raw events and extract trades.
    ///
    /// This is pure logic - no async, no I/O.
    pub fn process_block(&mut self, events: &stream::RawBlockEvents) -> BlockTrades {
        let mut trades = Vec::new();

        for event in events.events() {
            // Reset context at transaction boundary (pattern from exchange.rs)
            if self.prev_tx_index.is_some_and(|idx| idx < event.tx_index()) {
                self.order_context.take();
                self.pending_maker_fills.clear();
            }

            if let Some(trade) = self.process_event(event) {
                trades.push(trade);
            }

            self.prev_tx_index = Some(event.tx_index());
        }

        BlockTrades::new(events.instant(), trades)
    }

    /// Process a single event, potentially emitting a trade.
    fn process_event(&mut self, event: &stream::RawEvent) -> Option<TakerTrade> {
        match event.event() {
            ExchangeEvents::OrderRequest(e) => {
                let request_type: RequestType = e.orderType.into();
                // Only track context for order types that can have fills
                if let Some(side) = request_type.try_side() {
                    self.order_context = Some(OrderContext {
                        account_id: e.accountId.to(),
                        side,
                    });
                }
                None
            }
            ExchangeEvents::OrderBatchCompleted(_) => {
                self.order_context.take();
                self.pending_maker_fills.clear();
                None
            }
            ExchangeEvents::MakerOrderFilled(e) => {
                self.handle_maker_fill(event, e);
                None
            }
            ExchangeEvents::TakerOrderFilled(e) => self.handle_taker_fill(event, e),
            _ => None,
        }
    }

    fn handle_maker_fill(&mut self, event: &stream::RawEvent, e: &MakerOrderFilled) {
        let perp_id: types::PerpetualId = e.perpId.to();
        if let Some(converters) = self.config.perpetuals.get(&perp_id) {
            self.pending_maker_fills.push(PendingMakerFill {
                tx_hash: event.tx_hash(),
                log_index: event.log_index(),
                perpetual_id: perp_id,
                maker_account_id: e.accountId.to(),
                maker_order_id: e.orderId.to(),
                price: converters.price_converter.from_unsigned(e.pricePNS),
                size: converters.size_converter.from_unsigned(e.lotLNS),
                maker_fee: self.config.collateral_converter.from_unsigned(e.feeCNS),
            });
        }
    }

    fn handle_taker_fill(
        &mut self,
        event: &stream::RawEvent,
        e: &crate::abi::dex::Exchange::TakerOrderFilled,
    ) -> Option<TakerTrade> {
        let makers = std::mem::take(&mut self.pending_maker_fills);
        if makers.is_empty() {
            return None;
        }

        let ctx = self.order_context.as_ref()?;
        let taker_tx_hash = event.tx_hash();

        // Validate all maker fills have the same tx_hash as the taker fill
        // This ensures proper correlation within the same transaction
        if !makers.iter().all(|m| m.tx_hash == taker_tx_hash) {
            // Data corruption: maker fills from different transaction
            // Skip this trade to avoid incorrect correlations
            return None;
        }

        // All makers should have the same perpetual_id (from the same order request)
        let perpetual_id = makers.first()?.perpetual_id;

        Some(TakerTrade {
            tx_hash: taker_tx_hash,
            tx_index: event.tx_index(),
            perpetual_id,
            taker_account_id: ctx.account_id,
            taker_side: ctx.side,
            taker_fee: self.config.collateral_converter.from_unsigned(e.feeCNS),
            maker_fills: makers
                .into_iter()
                .map(|m| MakerFill {
                    log_index: m.log_index,
                    maker_account_id: m.maker_account_id,
                    maker_order_id: m.maker_order_id,
                    price: m.price,
                    size: m.size,
                    fee: m.maker_fee,
                })
                .collect(),
        })
    }
}

impl NormalizationConfig {
    /// Fetch normalization config from the chain.
    pub async fn fetch<P: Provider>(chain: &Chain, provider: &P) -> Result<Self, DexError> {
        let instance = ExchangeInstance::new(chain.exchange(), provider);

        // Fetch exchange info for collateral decimals
        let exchange_info = instance.getExchangeInfo().call().await?;
        let collateral_converter = num::Converter::new(exchange_info.collateralDecimals.to());

        // Fetch perpetual info for each perpetual
        let mut perpetuals = HashMap::new();
        for perp_id in chain.perpetuals() {
            let perp_info = instance
                .getPerpetualInfo(U256::from(*perp_id))
                .call()
                .await?;
            perpetuals.insert(
                *perp_id,
                PerpetualConverters {
                    price_converter: num::Converter::new(perp_info.priceDecimals.to()),
                    size_converter: num::Converter::new(perp_info.lotDecimals.to()),
                },
            );
        }

        Ok(Self {
            collateral_converter,
            perpetuals,
        })
    }
}

/// Start the trade listener.
///
/// Returns a receiver for trades and a handle to the background task.
/// The listener will stream all trades from the exchange starting from
/// the specified block, normalize them, and push them to the channel.
///
/// # Example
///
/// ```ignore
/// let (mut rx, handle) = fill::start(&chain, provider, from, tokio::time::sleep).await?;
///
/// while let Some(block_trades) = rx.recv().await {
///     for trade in &block_trades.trades {
///         println!("Taker {} {:?} on perp {} (fee: {})",
///             trade.taker_account_id, trade.taker_side,
///             trade.perpetual_id, trade.taker_fee);
///         for fill in &trade.maker_fills {
///             println!("  Maker {} @ {} (fee: {})", fill.size, fill.price, fill.fee);
///         }
///     }
/// }
/// ```
pub async fn start<P, S, SFut>(
    chain: &Chain,
    provider: P,
    from: types::StateInstant,
    sleep: S,
) -> Result<(TradeReceiver, tokio::task::JoinHandle<Result<(), DexError>>), DexError>
where
    P: Provider + Clone + Send + 'static,
    S: Fn(Duration) -> SFut + Copy + Send + 'static,
    SFut: Future<Output = ()> + Send,
{
    // Fetch normalization config
    let config = NormalizationConfig::fetch(chain, &provider).await?;

    let (tx, rx) = mpsc::channel(DEFAULT_CHANNEL_SIZE);

    let chain_clone = chain.clone();
    let handle =
        tokio::spawn(
            async move { run_listener(chain_clone, provider, from, sleep, config, tx).await },
        );

    Ok((TradeReceiver::new(rx), handle))
}

async fn run_listener<P, S, SFut>(
    chain: Chain,
    provider: P,
    from: types::StateInstant,
    sleep: S,
    config: NormalizationConfig,
    tx: mpsc::Sender<BlockTrades>,
) -> Result<(), DexError>
where
    P: Provider,
    S: Fn(Duration) -> SFut + Copy,
    SFut: Future<Output = ()>,
{
    let raw_stream = stream::raw(&chain, provider, from, sleep);
    futures::pin_mut!(raw_stream);

    let mut processor = TradeProcessor::new(config);

    while let Some(result) = raw_stream.next().await {
        let block_events = result?;

        // Pure processing - no async
        let block_trades = processor.process_block(&block_events);

        // Send trades (even if empty, for block progression tracking)
        if tx.send(block_trades).await.is_err() {
            // Receiver dropped, graceful shutdown
            break;
        }
    }

    Ok(())
}
