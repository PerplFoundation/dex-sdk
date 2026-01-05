use std::{collections::HashMap, num::NonZeroU16};

use alloy::{primitives::U256, providers::Provider};
use futures::{Stream, StreamExt};

use crate::{
    Chain,
    abi::dex::Exchange::{ExchangeEvents, ExchangeInstance, MakerOrderFilled},
    error::DexError,
    num, types,
};

pub type TradeEvent = types::EventContext<types::Trade>;
pub type BlockTrades = types::BlockEvents<TradeEvent>;

/// Returns stream of normalized trade events aggregated from the [`super::raw`] event stream,
/// batched per block.
///
/// Listens to `MakerOrderFilled` and `TakerOrderFilled` events, batches all
/// maker fills per taker into unified `Trade` events, normalizes
/// fixed-point values to decimals.
///
/// # Safety note
///
/// The returned stream is not cancellation-safe and should not be used within `select!`.
///
/// # Architecture
///
/// The module separates pure processing logic from async I/O:
///
/// - [`TradeProcessor`] - Pure, synchronous trade extraction from raw events
/// - [`NormalizationConfig`] - Configuration fetched once at startup
///
/// # Data Model
///
/// Each [`TradeEvent`] represents a single taker order execution that may have
/// matched against multiple maker orders. The `maker_fills` vector contains
/// all individual [`types::MakerFill`]s that occurred as part of this trade.
///
/// # Example
///
/// ```ignore
/// use perpl_sdk::{Chain, stream, types::StateInstant};
///
/// let chain = Chain::testnet();
/// let provider = /* setup provider */;
/// let from = StateInstant::new(latest_block, timestamp);
///
/// let raw_stream = stream::raw(
///     &chain,
///     provider.clone(),
///     types::StateInstant::new(block_num, 0),
///     tokio::time::sleep,
/// );
/// let mut trade_stream = pin!(stream::trade(&chain, provider, raw_stream).await.unwrap());
///
/// while let Some(Ok(block_trades)) = trade_stream.next().await {
///     if !block_trades.events().is_empty() {
///         println!(
///             "Block {} - {} trade(s):",
///             block_trades.instant().block_number(),
///             block_trades.events().len()
///         );
///         for event in block_trades.events() {
///             let trade = event.event();
///             println!(
///                 "  Taker {} {:?} {} @ {} on perp={} (fee: {})",
///                 trade.taker_account_id,
///                 trade.taker_side,
///                 trade.total_size(),
///                 trade.avg_price().unwrap_or_default(),
///                 trade.perpetual_id,
///                 trade.taker_fee,
///             );
///             for fill in &trade.maker_fills {
///                 println!(
///                     "    <- Maker {} order {} filled {} @ {} (fee: {})",
///                     fill.maker_account_id, fill.maker_order_id, fill.size, fill.price, fill.fee,
///                 );
///             }
///         }
///     }
/// }
/// ```
///
pub async fn trade<P>(
    chain: &Chain,
    provider: P,
    raw_events: impl Stream<Item = Result<super::RawBlockEvents, DexError>>,
) -> Result<impl Stream<Item = Result<BlockTrades, DexError>>, DexError>
where
    P: Provider,
{
    // Fetch normalization config
    let config = NormalizationConfig::fetch(chain, &provider).await?;
    // Setup trade processor
    let mut processor = TradeProcessor::new(config);

    let stream = raw_events.map(move |block_result| {
        block_result.map(|block_events| processor.process_block(&block_events))
    });

    Ok(stream)
}

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
    side: types::OrderSide,
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
    pub fn process_block(&mut self, events: &super::RawBlockEvents) -> BlockTrades {
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
    fn process_event(&mut self, event: &super::RawEvent) -> Option<TradeEvent> {
        match event.event() {
            ExchangeEvents::OrderRequest(e) => {
                let request_type: types::RequestType = e.orderType.into();
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

    fn handle_maker_fill(&mut self, event: &super::RawEvent, e: &MakerOrderFilled) {
        let perp_id: types::PerpetualId = e.perpId.to();
        if let Some(converters) = self.config.perpetuals.get(&perp_id) {
            self.pending_maker_fills.push(PendingMakerFill {
                tx_hash: event.tx_hash(),
                log_index: event.log_index(),
                perpetual_id: perp_id,
                maker_account_id: e.accountId.to(),
                maker_order_id: NonZeroU16::new(e.orderId.to()).expect("non-zero maker order ID"),
                price: converters.price_converter.from_unsigned(e.pricePNS),
                size: converters.size_converter.from_unsigned(e.lotLNS),
                maker_fee: self.config.collateral_converter.from_unsigned(e.feeCNS),
            });
        }
    }

    fn handle_taker_fill(
        &mut self,
        event: &super::RawEvent,
        e: &crate::abi::dex::Exchange::TakerOrderFilled,
    ) -> Option<TradeEvent> {
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

        Some(
            event.pass(types::Trade {
                perpetual_id,
                taker_account_id: ctx.account_id,
                taker_side: ctx.side,
                taker_fee: self.config.collateral_converter.from_unsigned(e.feeCNS),
                maker_fills: makers
                    .into_iter()
                    .map(|m| types::MakerFill {
                        log_index: m.log_index,
                        maker_account_id: m.maker_account_id,
                        maker_order_id: m.maker_order_id,
                        price: m.price,
                        size: m.size,
                        fee: m.maker_fee,
                    })
                    .collect(),
            }),
        )
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy::{
        providers::ProviderBuilder, rpc::client::RpcClient, transports::layers::RetryBackoffLayer,
    };
    use futures::StreamExt;

    use super::*;
    use crate::Chain;

    #[tokio::test]
    async fn test_stream_recent_blocks() {
        let client = RpcClient::builder()
            .layer(RetryBackoffLayer::new(10, 100, 200))
            .connect("https://testnet-rpc.monad.xyz")
            .await
            .unwrap();
        client.set_poll_interval(Duration::from_millis(100));
        let provider = ProviderBuilder::new().connect_client(client);

        let testnet = Chain::testnet();
        let block_num = provider.get_block_number().await.unwrap() + 1;
        let raw_stream = crate::stream::raw(
            &testnet,
            provider.clone(),
            types::StateInstant::new(block_num, 0),
            tokio::time::sleep,
        );

        let trade_stream = trade(&testnet, provider, raw_stream).await.unwrap();
        let block_trades = trade_stream.take(10).collect::<Vec<_>>().await;

        for bt in &block_trades {
            println!("block trades: {:?}", bt);
        }
    }
}
