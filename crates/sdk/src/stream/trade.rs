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

/// Returns stream of normalized trade events aggregated from the [`super::raw`]
/// event stream, batched per block.
///
/// Listens to `MakerOrderFilled` and `TakerOrderFilled` events, batches all
/// maker fills per taker into unified `Trade` events, normalizes
/// fixed-point values to decimals.
///
/// # Safety note
///
/// The returned stream is not cancellation-safe and should not be used within
/// `select!`.
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
    perpetual_id: types::PerpetualId,
    account_id: types::AccountId,
    request_id: types::RequestId,
    side: types::OrderSide,
}

/// Pending maker fill waiting for taker match.
struct PendingMakerFill {
    tx_hash: alloy::primitives::TxHash,
    log_index: u64,
    perpetual_id: types::PerpetualId,
    maker_account_id: types::AccountId,
    maker_order_id: types::OrderId,
    maker_client_order_id: Option<types::RequestId>,
    price: fastnum::UD64,
    size: fastnum::UD64,
    maker_fee: fastnum::UD64,
}

/// Trade processor - pure logic, no async.
pub struct TradeProcessor {
    config: NormalizationConfig,
    order_context: Option<OrderContext>,
    // Entries are retained after orders close and overwritten on ID reuse, so this can
    // hold up to 65,535 entries per configured perpetual.
    maker_client_order_ids: HashMap<(types::PerpetualId, types::OrderId), types::RequestId>,
    pending_maker_fills: Vec<PendingMakerFill>,
    prev_tx_index: Option<u64>,
}

impl TradeProcessor {
    /// Create a new trade processor with the given normalization config.
    pub fn new(config: NormalizationConfig) -> Self {
        Self {
            config,
            order_context: None,
            maker_client_order_ids: HashMap::new(),
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
                        perpetual_id: e.perpId.to(),
                        account_id: e.accountId.to(),
                        request_id: e.orderDescId.to(),
                        side,
                    });
                }
                None
            },
            ExchangeEvents::OrderBatchCompleted(_) => {
                self.order_context.take();
                self.pending_maker_fills.clear();
                None
            },
            ExchangeEvents::OrderPlaced(e) => {
                if let Some(context) = self.order_context.as_ref()
                    && self.config.perpetuals.contains_key(&context.perpetual_id)
                    && let Some(order_id) = NonZeroU16::new(e.orderId.to())
                {
                    self.maker_client_order_ids
                        .insert((context.perpetual_id, order_id), context.request_id);
                }
                None
            },
            ExchangeEvents::MakerOrderFilled(e) => {
                self.handle_maker_fill(event, e);
                None
            },
            ExchangeEvents::TakerOrderFilled(e) => self.handle_taker_fill(event, e),
            _ => None,
        }
    }

    fn handle_maker_fill(&mut self, event: &super::RawEvent, e: &MakerOrderFilled) {
        let perp_id: types::PerpetualId = e.perpId.to();
        let maker_order_id = NonZeroU16::new(e.orderId.to()).expect("non-zero maker order ID");
        if let Some(converters) = self.config.perpetuals.get(&perp_id) {
            let maker_client_order_id = self
                .maker_client_order_ids
                .get(&(perp_id, maker_order_id))
                .copied();
            self.pending_maker_fills.push(PendingMakerFill {
                tx_hash: event.tx_hash(),
                log_index: event.log_index(),
                perpetual_id: perp_id,
                maker_account_id: e.accountId.to(),
                maker_order_id,
                maker_client_order_id,
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
                taker_request_id: ctx.request_id,
                taker_side: ctx.side,
                taker_fee: self.config.collateral_converter.from_unsigned(e.feeCNS),
                maker_fills: makers
                    .into_iter()
                    .map(|m| types::MakerFill {
                        log_index: m.log_index,
                        maker_account_id: m.maker_account_id,
                        maker_order_id: m.maker_order_id,
                        maker_client_order_id: m.maker_client_order_id,
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
        let exchange_info = instance
            .getExchangeInfo()
            .call()
            .await
            .map_err(|err| DexError::Provider(err.into()))?;
        let collateral_converter = num::Converter::new(exchange_info.collateralDecimals.to());

        // Fetch perpetual info for each perpetual
        let mut perpetuals = HashMap::new();
        for perp_id in chain.perpetuals() {
            let perp_info = instance
                .getPerpetualInfo(U256::from(*perp_id))
                .call()
                .await
                .map_err(|err| DexError::Provider(err.into()))?;
            perpetuals.insert(
                *perp_id,
                PerpetualConverters {
                    price_converter: num::Converter::new(perp_info.priceDecimals.to()),
                    size_converter: num::Converter::new(perp_info.lotDecimals.to()),
                },
            );
        }

        Ok(Self { collateral_converter, perpetuals })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy::{
        primitives::{I256, TxHash},
        providers::ProviderBuilder,
        rpc::client::RpcClient,
        transports::layers::RetryBackoffLayer,
    };
    use futures::StreamExt;

    use super::*;
    use crate::{
        Chain,
        abi::dex::Exchange::{OrderPlaced, OrderRequest, TakerOrderFilled},
    };

    fn order_request(
        perpetual_id: types::PerpetualId,
        account_id: types::AccountId,
        request_id: types::RequestId,
        order_type: u8,
    ) -> ExchangeEvents {
        ExchangeEvents::OrderRequest(OrderRequest {
            perpId: U256::from(perpetual_id),
            accountId: U256::from(account_id),
            orderDescId: U256::from(request_id),
            orderId: U256::ZERO,
            orderType: order_type,
            pricePNS: U256::from(100),
            lotLNS: U256::from(1),
            expiryBlock: U256::ZERO,
            postOnly: false,
            fillOrKill: false,
            immediateOrCancel: false,
            maxMatches: U256::ZERO,
            leverageHdths: U256::ZERO,
            lastExecutionBlock: U256::ZERO,
            amountCNS: U256::ZERO,
            maxNegPnlCollatBPS: U256::ZERO,
            gasLeft: U256::ZERO,
        })
    }

    fn normalization_config(perpetual_id: types::PerpetualId) -> NormalizationConfig {
        let converter = num::Converter::new(0);
        NormalizationConfig {
            collateral_converter: converter,
            perpetuals: HashMap::from([(
                perpetual_id,
                PerpetualConverters { price_converter: converter, size_converter: converter },
            )]),
        }
    }

    #[test]
    fn client_order_ids_ignore_unconfigured_perpetuals() {
        let mut processor = TradeProcessor::new(normalization_config(1));
        processor.order_context = Some(OrderContext {
            perpetual_id: 2,
            account_id: 7,
            request_id: 42,
            side: types::OrderSide::Ask,
        });
        let event = crate::stream::RawEvent::new(
            TxHash::ZERO,
            0,
            0,
            ExchangeEvents::OrderPlaced(OrderPlaced {
                orderId: U256::from(9),
                lotLNS: U256::from(1),
                lockedBalanceCNS: U256::ZERO,
                amountCNS: I256::ZERO,
                balanceCNS: U256::ZERO,
            }),
        );

        _ = processor.process_event(&event);

        assert!(processor.maker_client_order_ids.is_empty());
    }

    #[test]
    fn maker_fill_includes_observed_client_order_id() {
        const PERPETUAL_ID: types::PerpetualId = 1;
        const MAKER_ACCOUNT_ID: types::AccountId = 7;
        const MAKER_CLIENT_ORDER_ID: types::RequestId = 42;
        const TAKER_REQUEST_ID: types::RequestId = 84;
        const MAKER_ORDER_ID: u16 = 9;

        let mut processor = TradeProcessor::new(normalization_config(PERPETUAL_ID));
        let maker_tx_hash = TxHash::from([1_u8; 32]);
        let taker_tx_hash = TxHash::from([2_u8; 32]);
        let events = crate::stream::RawBlockEvents::new(
            types::StateInstant::new(1, 0),
            vec![
                crate::stream::RawEvent::new(
                    maker_tx_hash,
                    0,
                    0,
                    order_request(PERPETUAL_ID, MAKER_ACCOUNT_ID, MAKER_CLIENT_ORDER_ID, 1),
                ),
                crate::stream::RawEvent::new(
                    maker_tx_hash,
                    0,
                    1,
                    ExchangeEvents::OrderPlaced(OrderPlaced {
                        orderId: U256::from(MAKER_ORDER_ID),
                        lotLNS: U256::from(1),
                        lockedBalanceCNS: U256::ZERO,
                        amountCNS: I256::ZERO,
                        balanceCNS: U256::ZERO,
                    }),
                ),
                crate::stream::RawEvent::new(
                    taker_tx_hash,
                    1,
                    2,
                    order_request(PERPETUAL_ID, 8, TAKER_REQUEST_ID, 0),
                ),
                crate::stream::RawEvent::new(
                    taker_tx_hash,
                    1,
                    3,
                    ExchangeEvents::MakerOrderFilled(MakerOrderFilled {
                        perpId: U256::from(PERPETUAL_ID),
                        accountId: U256::from(MAKER_ACCOUNT_ID),
                        orderId: U256::from(MAKER_ORDER_ID),
                        pricePNS: U256::from(100),
                        lotLNS: U256::from(1),
                        feeCNS: U256::from(1),
                        lockedBalanceCNS: U256::ZERO,
                        amountCNS: I256::ZERO,
                        balanceCNS: U256::ZERO,
                    }),
                ),
                crate::stream::RawEvent::new(
                    taker_tx_hash,
                    1,
                    4,
                    ExchangeEvents::TakerOrderFilled(TakerOrderFilled {
                        entryPricePNS: U256::from(100),
                        collatPricePNS: U256::from(100),
                        pnlPricePNS: U256::from(100),
                        lotLNS: U256::from(1),
                        feeCNS: U256::from(1),
                        amountCNS: I256::ZERO,
                        balanceCNS: U256::ZERO,
                    }),
                ),
            ],
        );

        let block_trades = processor.process_block(&events);
        let trade = block_trades.events().first().expect("trade exists").event();
        let maker_fill = trade.maker_fills.first().expect("maker fill exists");

        assert_eq!(maker_fill.maker_client_order_id, Some(MAKER_CLIENT_ORDER_ID));
    }

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
