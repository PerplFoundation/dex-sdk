use std::{collections::HashMap, num::NonZeroU16};

use alloy::{eips::BlockId, primitives::U256, providers::Provider};
use futures::{Stream, StreamExt};

use crate::{
    Chain,
    abi::dex::Exchange::{ExchangeEvents, ExchangeInstance},
    error::DexError,
    num, state, types,
};

pub type TradeEvent = types::EventContext<types::Trade>;
pub type BlockTrades = types::BlockEvents<TradeEvent>;

/// Returns stream of normalized trade events aggregated from the [`super::raw`]
/// event stream, batched per block.
///
/// Listens to `MakerOrderFilledV2` and `TakerOrderFilledV2` events (and their
/// V1 predecessors when replaying pre-v1.1.7.4 history), batches all maker
/// fills per taker into unified `Trade` events, normalizes fixed-point values
/// to decimals and recovers builder attribution from the order requests.
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
    P: Provider + Clone,
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
    builder: Option<types::BuilderAttribution>,
}

/// Pending maker fill waiting for taker match.
struct PendingMakerFill {
    tx_hash: alloy::primitives::TxHash,
    log_index: u64,
    perpetual_id: types::PerpetualId,
    maker_account_id: types::AccountId,
    maker_order_id: types::OrderId,
    maker_client_order_id: Option<types::RequestId>,
    maker_builder: Option<types::BuilderAttribution>,
    price: fastnum::UD64,
    size: fastnum::UD64,
    maker_fee: fastnum::UD64,
    maker_builder_fee: fastnum::UD64,
}

/// Raw maker fill data, common to the V1 and V2 `MakerOrderFilled*` events.
struct RawMakerFill {
    perp_id: U256,
    account_id: U256,
    order_id: U256,
    price_pns: U256,
    lot_lns: U256,
    fee_cns: U256,
    builder_fee_cns: U256,
}

/// Trade processor - pure logic, no async.
pub struct TradeProcessor {
    config: NormalizationConfig,
    order_context: Option<OrderContext>,
    // Entries are retained after orders close and overwritten on ID reuse, so this can
    // hold up to 65,535 entries per configured perpetual.
    maker_orders: HashMap<(types::PerpetualId, types::OrderId), PlacedOrder>,
    pending_maker_fills: Vec<PendingMakerFill>,
    prev_tx_index: Option<u64>,
}

/// Attribution of an order observed being placed, recovered at fill time.
#[derive(Clone, Copy)]
struct PlacedOrder {
    client_order_id: types::RequestId,
    builder: Option<types::BuilderAttribution>,
}

impl TradeProcessor {
    /// Create a new trade processor with the given normalization config.
    pub fn new(config: NormalizationConfig) -> Self {
        Self {
            config,
            order_context: None,
            maker_orders: HashMap::new(),
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
            // V1 order/fill events are never emitted by contract v1.1.7.4+, but
            // stay handled to keep historical replay working
            ExchangeEvents::OrderRequest(e) => {
                self.track_order_request(e.perpId, e.accountId, e.orderDescId, e.orderType, None);
                None
            },
            ExchangeEvents::OrderRequestV2(e) => {
                self.track_order_request(
                    e.perpId,
                    e.accountId,
                    e.orderDescId,
                    e.orderType,
                    types::BuilderAttribution::decode(&e.extension)
                        .ok()
                        .flatten(),
                );
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
                    self.maker_orders.insert(
                        (context.perpetual_id, order_id),
                        PlacedOrder {
                            client_order_id: context.request_id,
                            builder: context.builder,
                        },
                    );
                }
                None
            },
            ExchangeEvents::MakerOrderFilled(e) => {
                self.handle_maker_fill(
                    event,
                    RawMakerFill {
                        perp_id: e.perpId,
                        account_id: e.accountId,
                        order_id: e.orderId,
                        price_pns: e.pricePNS,
                        lot_lns: e.lotLNS,
                        fee_cns: e.feeCNS,
                        builder_fee_cns: U256::ZERO,
                    },
                );
                None
            },
            ExchangeEvents::MakerOrderFilledV2(e) => {
                self.handle_maker_fill(
                    event,
                    RawMakerFill {
                        perp_id: e.perpId,
                        account_id: e.accountId,
                        order_id: e.orderId,
                        price_pns: e.pricePNS,
                        lot_lns: e.lotLNS,
                        fee_cns: e.feeCNS,
                        builder_fee_cns: e.builderFeeCNS,
                    },
                );
                None
            },
            ExchangeEvents::TakerOrderFilled(e) => {
                self.handle_taker_fill(event, e.feeCNS, U256::ZERO)
            },
            ExchangeEvents::TakerOrderFilledV2(e) => {
                self.handle_taker_fill(event, e.feeCNS, e.builderFeeCNS)
            },
            _ => None,
        }
    }

    fn track_order_request(
        &mut self,
        perp_id: U256,
        account_id: U256,
        request_id: U256,
        order_type: u8,
        builder: Option<types::BuilderAttribution>,
    ) {
        let request_type: types::RequestType = order_type.into();
        // Only track context for order types that can have fills
        if let Some(side) = request_type.try_side() {
            self.order_context = Some(OrderContext {
                perpetual_id: perp_id.to(),
                account_id: account_id.to(),
                request_id: request_id.to(),
                side,
                builder,
            });
        }
    }

    fn handle_maker_fill(&mut self, event: &super::RawEvent, fill: RawMakerFill) {
        let perp_id: types::PerpetualId = fill.perp_id.to();
        let maker_order_id = NonZeroU16::new(fill.order_id.to()).expect("non-zero maker order ID");
        if let Some(converters) = self.config.perpetuals.get(&perp_id) {
            let maker_order = self.maker_orders.get(&(perp_id, maker_order_id)).copied();
            self.pending_maker_fills.push(PendingMakerFill {
                tx_hash: event.tx_hash(),
                log_index: event.log_index(),
                perpetual_id: perp_id,
                maker_account_id: fill.account_id.to(),
                maker_order_id,
                maker_client_order_id: maker_order.map(|o| o.client_order_id),
                maker_builder: maker_order.and_then(|o| o.builder),
                price: converters.price_converter.from_unsigned(fill.price_pns),
                size: converters.size_converter.from_unsigned(fill.lot_lns),
                maker_fee: self.config.collateral_converter.from_unsigned(fill.fee_cns),
                maker_builder_fee: self
                    .config
                    .collateral_converter
                    .from_unsigned(fill.builder_fee_cns),
            });
        }
    }

    fn handle_taker_fill(
        &mut self,
        event: &super::RawEvent,
        fee_cns: U256,
        builder_fee_cns: U256,
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
                taker_fee: self.config.collateral_converter.from_unsigned(fee_cns),
                taker_builder: ctx.builder,
                taker_builder_fee: self
                    .config
                    .collateral_converter
                    .from_unsigned(builder_fee_cns),
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
                        builder: m.maker_builder,
                        builder_fee: m.maker_builder_fee,
                    })
                    .collect(),
            }),
        )
    }
}

impl NormalizationConfig {
    /// Fetch normalization config from the chain.
    ///
    /// Tracks every perpetual listed on the exchange unless the chain
    /// configuration names a subset, see [`Chain::perpetuals`].
    pub async fn fetch<P: Provider + Clone>(chain: &Chain, provider: &P) -> Result<Self, DexError> {
        let instance = ExchangeInstance::new(chain.exchange(), provider);

        // Fetch exchange info for collateral decimals
        let exchange_info = instance
            .getExchangeInfo()
            .call()
            .await
            .map_err(|err| DexError::Provider(err.into()))?;
        let collateral_converter = num::Converter::new(exchange_info.collateralDecimals.to());

        let perpetual_ids = if chain.perpetuals().is_empty() {
            state::listed_perpetuals(chain, provider.clone(), BlockId::latest()).await?
        } else {
            chain.perpetuals().to_vec()
        };

        // Fetch perpetual info for each perpetual
        let mut perpetuals = HashMap::new();
        for perp_id in &perpetual_ids {
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
        primitives::I256, providers::ProviderBuilder, rpc::client::RpcClient,
        transports::layers::RetryBackoffLayer,
    };
    use fastnum::udec64;
    use futures::StreamExt;

    use super::*;
    use crate::{
        Chain,
        abi::dex::Exchange::{MakerOrderFilledV2, OrderPlaced, OrderRequestV2, TakerOrderFilledV2},
        stream::RawEvent,
    };

    fn order_request(
        perpetual_id: types::PerpetualId,
        account_id: types::AccountId,
        request_id: types::RequestId,
        order_type: u8,
        builder: Option<types::BuilderAttribution>,
    ) -> ExchangeEvents {
        ExchangeEvents::OrderRequestV2(OrderRequestV2 {
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
            extension: builder
                .map(|b| b.encode().expect("fee within range"))
                .unwrap_or_default(),
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
            builder: None,
        });
        let event = RawEvent::empty(ExchangeEvents::OrderPlaced(OrderPlaced {
            orderId: U256::from(9),
            lotLNS: U256::from(1),
            lockedBalanceCNS: U256::ZERO,
            amountCNS: I256::ZERO,
            balanceCNS: U256::ZERO,
        }));

        _ = processor.process_event(&event);

        assert!(processor.maker_orders.is_empty());
    }

    /// Drives a single maker-vs-taker match through the processor, attributing
    /// each side to the given builder, and returns the resulting trade.
    fn one_match_trade(
        maker_builder: Option<types::BuilderAttribution>,
        maker_builder_fee: u64,
        taker_builder: Option<types::BuilderAttribution>,
        taker_builder_fee: u64,
    ) -> types::Trade {
        const PERPETUAL_ID: types::PerpetualId = 1;
        const MAKER_ACCOUNT_ID: types::AccountId = 7;
        const MAKER_CLIENT_ORDER_ID: types::RequestId = 42;
        const TAKER_REQUEST_ID: types::RequestId = 84;
        const MAKER_ORDER_ID: u16 = 9;

        let mut processor = TradeProcessor::new(normalization_config(PERPETUAL_ID));
        _ = processor.process_event(&RawEvent::empty(order_request(
            PERPETUAL_ID,
            MAKER_ACCOUNT_ID,
            MAKER_CLIENT_ORDER_ID,
            1,
            maker_builder,
        )));
        _ = processor.process_event(&RawEvent::empty(ExchangeEvents::OrderPlaced(OrderPlaced {
            orderId: U256::from(MAKER_ORDER_ID),
            lotLNS: U256::from(1),
            lockedBalanceCNS: U256::ZERO,
            amountCNS: I256::ZERO,
            balanceCNS: U256::ZERO,
        })));
        _ = processor.process_event(&RawEvent::empty(order_request(
            PERPETUAL_ID,
            8,
            TAKER_REQUEST_ID,
            0,
            taker_builder,
        )));
        _ = processor.process_event(&RawEvent::empty(ExchangeEvents::MakerOrderFilledV2(
            MakerOrderFilledV2 {
                perpId: U256::from(PERPETUAL_ID),
                accountId: U256::from(MAKER_ACCOUNT_ID),
                orderId: U256::from(MAKER_ORDER_ID),
                pricePNS: U256::from(100),
                lotLNS: U256::from(1),
                feeCNS: U256::from(2),
                lockedBalanceCNS: U256::ZERO,
                amountCNS: I256::ZERO,
                balanceCNS: U256::ZERO,
                builderId: U256::from(maker_builder.map(|b| b.builder_id()).unwrap_or_default()),
                builderFeeCNS: U256::from(maker_builder_fee),
            },
        )));
        processor
            .process_event(&RawEvent::empty(ExchangeEvents::TakerOrderFilledV2(
                TakerOrderFilledV2 {
                    entryPricePNS: U256::from(100),
                    collatPricePNS: U256::from(100),
                    pnlPricePNS: U256::from(100),
                    lotLNS: U256::from(1),
                    feeCNS: U256::from(3),
                    amountCNS: I256::ZERO,
                    balanceCNS: U256::ZERO,
                    builderId: U256::from(
                        taker_builder.map(|b| b.builder_id()).unwrap_or_default(),
                    ),
                    builderFeeCNS: U256::from(taker_builder_fee),
                },
            )))
            .expect("trade exists")
            .event()
            .clone()
    }

    #[test]
    fn maker_fill_includes_observed_client_order_id() {
        let trade = one_match_trade(None, 0, None, 0);
        let maker_fill = trade.maker_fills.first().expect("maker fill exists");

        assert_eq!(maker_fill.maker_client_order_id, Some(42));
        assert_eq!(maker_fill.builder, None);
        assert_eq!(trade.total_builder_fees(), udec64!(0));
    }

    #[test]
    fn builder_attribution_recovered_from_order_requests() {
        let maker_builder = types::BuilderAttribution::new(3, udec64!(0.0005));
        let taker_builder = types::BuilderAttribution::new(4, udec64!(0.001));
        let trade = one_match_trade(Some(maker_builder), 1, Some(taker_builder), 2);

        // Attribution rides on the request that placed each order...
        let maker_fill = trade.maker_fills.first().expect("maker fill exists");
        assert_eq!(maker_fill.builder, Some(maker_builder));
        assert_eq!(trade.taker_builder, Some(taker_builder));

        // ...while the fee earned comes from the fill events, and is part of the
        // reported fee rather than additional to it.
        assert_eq!(maker_fill.builder_fee, udec64!(1));
        assert_eq!(trade.taker_builder_fee, udec64!(2));
        assert_eq!(trade.total_builder_fees(), udec64!(3));
        assert_eq!(trade.builder_total(3), udec64!(1));
        assert_eq!(trade.builder_total(4), udec64!(2));
        assert_eq!(trade.builder_total(5), udec64!(0));
        assert!(maker_fill.builder_fee < maker_fill.fee);
        assert!(trade.taker_builder_fee < trade.taker_fee);
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
