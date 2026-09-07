use std::{
    fmt::Display,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::{
    primitives::{Address, Bytes, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};
use fastnum::{UD64, UD128};

use super::*;
use crate::{abi::dex::Exchange::OrderDesc, error::DexError, num, state};

/// Type of the order request.
///
/// * [`RequestType::OpenLong`] is used to open a long position (or to decrease,
///   close, or invert a long position). The only restrictions applied are the
///   user account must have sufficient collateral available.
/// * [`RequestType::OpenShort`] is used to open a short position (or to
///   decrease, close, or invert a short position). The only restrictions
///   applied are the user account must have sufficient collateral available.
/// * [`RequestType::CloseLong`] is a reduce only order type and can only be
///   used to close all or part of an existing long position on the perpetual
///   contract.
/// * [`RequestType::CloseShort`] is a reduce only order type and can only be
///   used to close all or part of an existing short position on the perpetual
///   contract.
/// * [`RequestType::Cancel`] is used to cancel an existing order on the
///   perpetual contract's order book.
/// * [`RequestType::IncreasePositionCollateral`] is an operation to increase
///   the collateral of an existing position in the event that it has
///   insufficient margin or the account holder wishes to reduce leverage.
/// * [`RequestType::Change`] is an operation to change parameters of an
///   existing order, gas-efficiently.
#[derive(Clone, Copy, Debug)]
pub enum RequestType {
    OpenLong,
    OpenShort,
    CloseLong,
    CloseShort,
    Cancel,
    IncreasePositionCollateral,
    Change,
}

/// Request to post/modify an order.
#[derive(Clone, derive_more::Debug)]
pub struct OrderRequest {
    request_id: RequestId,
    perp_id: PerpetualId,
    r#type: RequestType,
    order_id: Option<OrderId>,
    #[debug("{price}")]
    price: UD64,
    #[debug("{size}")]
    size: UD64,
    expiry_block: Option<u64>,
    post_only: bool,
    fill_or_kill: bool,
    immediate_or_cancel: bool,
    max_matches: Option<u32>,
    #[debug("{leverage}")]
    leverage: UD64,
    last_exec_block: Option<u64>,
    amount: Option<UD128>,
    max_neg_pnl_collat_bps: u16,
    builder: Option<BuilderAttribution>,
}

impl OrderRequest {
    /// Create a new order request with provided parameters.
    ///
    /// Provided [`request_id`] is stored as [`client_order_id`] once the order
    /// gets placed.
    ///
    /// Use [`Self::prepare_v2`] to get an [`OrderDesc`] with its order
    /// extension and then issue transactions with
    /// [`crate::abi::dex::Exchange::ExchangeInstance::execOrdersV2`] calls, or
    /// [`Self::prepare`] for the builder-blind V1
    /// [`crate::abi::dex::Exchange::ExchangeInstance::execOrders`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: RequestId,
        perp_id: PerpetualId,
        r#type: RequestType,
        order_id: Option<OrderId>,
        price: UD64,
        size: UD64,
        expiry_block: Option<u64>,
        post_only: bool,
        fill_or_kill: bool,
        immediate_or_cancel: bool,
        max_matches: Option<u32>,
        leverage: UD64,
        last_exec_block: Option<u64>,
        amount: Option<UD128>,
        max_neg_pnl_collat_bps: u16,
    ) -> Self {
        Self {
            request_id,
            perp_id,
            r#type,
            order_id,
            price,
            size,
            expiry_block,
            post_only,
            fill_or_kill,
            immediate_or_cancel,
            max_matches,
            leverage,
            last_exec_block,
            amount,
            max_neg_pnl_collat_bps,
            builder: None,
        }
    }

    /// Attributes the order to a builder, which charges its own additive fee on
    /// the size the order adds.
    ///
    /// Only the V2 entrypoints carry attribution: use [`Self::prepare_v2`] to
    /// get the corresponding order extension envelope. Attribution is *silently
    /// dropped* by [`Self::prepare`], as the V1 entrypoints have nothing to
    /// carry it in.
    pub fn with_builder(mut self, builder: BuilderAttribution) -> Self {
        self.builder = Some(builder);
        self
    }

    /// Builder attribution of the request, if any.
    pub fn builder_attribution(&self) -> Option<BuilderAttribution> { self.builder }

    /// Prepare order request for execution via the V1 entrypoints
    /// (`execOrder`/`execOrders`), which cannot carry builder attribution.
    ///
    /// # Panics
    ///
    /// If the perpetual contract of the request is not tracked by `exchange`.
    pub fn prepare(&self, exchange: &state::Exchange) -> OrderDesc {
        let perp = exchange
            .perpetuals()
            .get(&self.perp_id)
            .expect("known perpetual");
        self.to_order_desc(
            perp.price_converter(),
            perp.size_converter(),
            perp.leverage_converter(),
            Some(exchange.collateral_converter()),
        )
    }

    /// Prepare order request for execution via the V2 entrypoints
    /// (`execOrderV2`/`execOrdersV2`), returning the order descriptor along
    /// with its order extension envelope.
    ///
    /// The envelope is empty for a request without builder attribution, which
    /// is the V1-identical fast path on-chain. A batch where no order
    /// carries attribution can omit the `extensions` array entirely.
    ///
    /// Fails if the request carries builder attribution the deployed contract
    /// does not support, or a builder fee rate the contract's decoder would
    /// reject - which reverts `execOrderV2` and skips the order on the batched
    /// path.
    pub fn prepare_v2(&self, exchange: &state::Exchange) -> Result<(OrderDesc, Bytes), DexError> {
        let perp = exchange
            .perpetuals()
            .get(&self.perp_id)
            .ok_or(DexError::PerpetualNotTracked(self.perp_id))?;
        let extension = match self.builder {
            None => Bytes::new(),
            Some(builder) => {
                if !exchange.features().builder_attribution() {
                    return Err(DexError::UnsupportedByContract(
                        "builder attribution",
                        exchange.features(),
                    ));
                }
                builder.encode()?
            },
        };
        Ok((
            self.to_order_desc(
                perp.price_converter(),
                perp.size_converter(),
                perp.leverage_converter(),
                Some(exchange.collateral_converter()),
            ),
            extension,
        ))
    }

    /// Order extension envelope of the request, empty without builder
    /// attribution.
    pub fn to_order_extension(&self) -> Result<Bytes, OrderExtensionError> {
        self.builder
            .map(|builder| builder.encode())
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn to_order_desc(
        &self,
        price_converter: num::Converter,
        size_converter: num::Converter,
        leverage_converter: num::Converter,
        collateral_converter: Option<num::Converter>,
    ) -> OrderDesc {
        OrderDesc {
            orderDescId: U256::from(self.request_id),
            perpId: U256::from(self.perp_id),
            orderType: self.r#type as u8,
            orderId: U256::from(self.order_id.map(|id| id.get()).unwrap_or(0)),
            pricePNS: price_converter.to_unsigned(self.price),
            lotLNS: size_converter.to_unsigned(self.size),
            expiryBlock: U256::from(self.expiry_block.unwrap_or_default()),
            postOnly: self.post_only,
            fillOrKill: self.fill_or_kill,
            immediateOrCancel: self.immediate_or_cancel,
            maxMatches: U256::from(self.max_matches.unwrap_or_default()),
            leverageHdths: leverage_converter.to_unsigned(self.leverage),
            lastExecutionBlock: U256::from(self.last_exec_block.unwrap_or_default()),
            amountCNS: self
                .amount
                .zip(collateral_converter)
                .map(|(a, conv)| conv.to_unsigned(a))
                .unwrap_or_default(),
            maxNegPnlCollatBPS: U256::from(self.max_neg_pnl_collat_bps),
        }
    }
}

impl From<u8> for RequestType {
    fn from(value: u8) -> Self {
        match value {
            0 => RequestType::OpenLong,
            1 => RequestType::OpenShort,
            2 => RequestType::CloseLong,
            3 => RequestType::CloseShort,
            4 => RequestType::Cancel,
            5 => RequestType::IncreasePositionCollateral,
            6 => RequestType::Change,
            _ => unreachable!(),
        }
    }
}

impl RequestType {
    /// Request type that posts on `side`, reducing an existing position
    /// rather than opening one when `reduce_only`.
    ///
    /// The exchange has no side flag: the request type carries both the
    /// direction and whether the order may only reduce. This is the inverse of
    /// [`Self::try_side`].
    pub fn from_side(side: OrderSide, reduce_only: bool) -> Self {
        match (side, reduce_only) {
            (OrderSide::Bid, false) => RequestType::OpenLong,
            (OrderSide::Ask, false) => RequestType::OpenShort,
            (OrderSide::Ask, true) => RequestType::CloseLong,
            (OrderSide::Bid, true) => RequestType::CloseShort,
        }
    }

    /// Returns the order side for this request type, if applicable.
    ///
    /// Returns `Some(side)` for order-placing types (OpenLong, OpenShort,
    /// CloseLong, CloseShort). Returns `None` for Cancel,
    /// IncreasePositionCollateral, and Change.
    pub fn try_side(&self) -> Option<OrderSide> {
        match self {
            RequestType::OpenLong | RequestType::CloseShort => Some(OrderSide::Bid),
            RequestType::OpenShort | RequestType::CloseLong => Some(OrderSide::Ask),
            _ => None,
        }
    }
}

impl From<RequestType> for OrderType {
    fn from(value: RequestType) -> Self {
        match value {
            RequestType::OpenLong => OrderType::OpenLong,
            RequestType::OpenShort => OrderType::OpenShort,
            RequestType::CloseLong => OrderType::CloseLong,
            RequestType::CloseShort => OrderType::CloseShort,
            _ => unreachable!(),
        }
    }
}

/// Default cap on the additional collateral the exchange may draw to cover a
/// position's negative unrealized PnL on a fill, in basis points of notional.
///
/// What [`OrderRequestBuilder`] posts when the caller does not pick one. Zero
/// is valid, and stricter: it lets the exchange draw nothing.
pub const DEFAULT_MAX_NEG_PNL_COLLAT_BPS: u16 = 1000;

/// Field of an order request a fault refers to.
///
/// Carried by [`OrderRequestError::Precision`] instead of a rendered name, so
/// a caller can name the field in the terms *its* own users typed - a CLI
/// flag, a form field, a JSON key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderField {
    Price,
    Size,
    Leverage,
}

/// Order request the exchange would reject, caught before anything is signed.
///
/// Every variant is a state the contract would revert on - except
/// [`Self::Precision`], which it would not: it truncates. Losing digits from a
/// price the caller typed is the one failure worth being noisy about.
#[derive(Clone, Debug, thiserror::Error)]
pub enum OrderRequestError {
    #[error("{0} and {1} contradict each other")]
    ContradictoryFlags(&'static str, &'static str),

    #[error("the exchange is halted")]
    ExchangeHalted,

    #[error("leverage {requested} exceeds the maximum of {max} on perpetual {perp}")]
    LeverageTooHigh { perp: PerpetualId, requested: UD64, max: UD64 },

    #[error("perpetual {0} is paused")]
    PerpetualPaused(PerpetualId),

    #[error(
        "{field} {value} carries more precision than perpetual's {decimals} decimal place(s) \
         allows; it would become {rescaled}"
    )]
    Precision { field: OrderField, value: UD64, decimals: u8, rescaled: UD64 },
}

impl OrderRequest {
    /// Builds a request from values in *human* units - `65432.1`, not the
    /// fixed-point integer the contract stores - validated against a snapshot.
    ///
    /// The one construction path worth taking unless the values are already
    /// scaled to the perpetual's own precision; see [`OrderRequestBuilder`].
    pub fn builder(
        perp_id: PerpetualId,
        r#type: RequestType,
        price: UD64,
        size: UD64,
    ) -> OrderRequestBuilder {
        OrderRequestBuilder {
            perp_id,
            r#type,
            price,
            size,
            order_id: None,
            request_id: None,
            leverage: None,
            expiry_block: None,
            post_only: false,
            fill_or_kill: false,
            immediate_or_cancel: false,
            max_matches: None,
            last_exec_block: None,
            amount: None,
            max_neg_pnl_collat_bps: DEFAULT_MAX_NEG_PNL_COLLAT_BPS,
            builder: None,
        }
    }

    pub fn perp_id(&self) -> PerpetualId { self.perp_id }

    /// Client order ID the request is tagged with.
    pub fn request_id(&self) -> RequestId { self.request_id }

    pub fn request_type(&self) -> RequestType { self.r#type }

    /// Limit price, in human units and the perpetual's own precision.
    pub fn price(&self) -> UD64 { self.price }

    /// Order size, in human units and the perpetual's own precision.
    pub fn size(&self) -> UD64 { self.size }

    /// Leverage the position is opened at, resolved: a request built by
    /// [`OrderRequestBuilder`] carries the perpetual's maximum where the
    /// caller named none.
    pub fn leverage(&self) -> UD64 { self.leverage }

    pub fn expiry_block(&self) -> Option<u64> { self.expiry_block }

    pub fn post_only(&self) -> bool { self.post_only }

    pub fn fill_or_kill(&self) -> bool { self.fill_or_kill }

    pub fn immediate_or_cancel(&self) -> bool { self.immediate_or_cancel }

    /// Transaction executing this request on `exchange`, signed and sent by
    /// `from` - see [`crate::exec::Call`] for the steps from here.
    pub fn to_transaction_request(
        &self,
        exchange: &state::Exchange,
        from: Address,
    ) -> Result<TransactionRequest, DexError> {
        crate::exec::orders_transaction(exchange, std::slice::from_ref(self), true, from)
    }

    /// This request as a staged call: build, simulate, send, wait.
    ///
    /// `provider` has to carry the wallet that signs for `from` - see
    /// [`crate::exec::Call::new`].
    pub fn call<P: Provider>(
        &self,
        exchange: &state::Exchange,
        provider: P,
        from: Address,
    ) -> Result<crate::exec::Call<P>, DexError> {
        Ok(crate::exec::Call::new(provider, self.to_transaction_request(exchange, from)?))
    }
}

/// Builds an [`OrderRequest`] from values in human units, checking it against
/// a snapshot before anything is signed.
///
/// The checks are the ones the contract would otherwise apply on chain, where
/// the revert reason is far less legible than an [`OrderRequestError`] - plus
/// precision, which the contract does not check at all.
///
/// Every optional setter takes either the value or an [`Option`] of it, so
/// arguments that arrive already optional need no unwrapping:
///
/// ```no_run
/// # use perpl_sdk::{state, types::{OrderRequest, RequestType}};
/// # fn f(exchange: &state::Exchange, leverage: Option<fastnum::UD64>) {
/// let request = OrderRequest::builder(
///     1,
///     RequestType::OpenLong,
///     fastnum::udec64!(65432.1),
///     fastnum::udec64!(0.001),
/// )
/// .leverage(leverage)
/// .post_only(true)
/// .build(exchange)
/// .expect("a valid order");
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct OrderRequestBuilder {
    perp_id: PerpetualId,
    r#type: RequestType,
    price: UD64,
    size: UD64,
    order_id: Option<OrderId>,
    request_id: Option<RequestId>,
    leverage: Option<UD64>,
    expiry_block: Option<u64>,
    post_only: bool,
    fill_or_kill: bool,
    immediate_or_cancel: bool,
    max_matches: Option<u32>,
    last_exec_block: Option<u64>,
    amount: Option<UD128>,
    max_neg_pnl_collat_bps: u16,
    builder: Option<BuilderAttribution>,
}

impl OrderRequestBuilder {
    /// Leverage to open the position at [default: the perpetual's maximum].
    pub fn leverage(mut self, leverage: impl Into<Option<UD64>>) -> Self {
        self.leverage = leverage.into();
        self
    }

    /// Client order ID to tag the request with [default: milliseconds since
    /// the epoch, which keeps a session's orders distinguishable in the event
    /// stream].
    pub fn request_id(mut self, request_id: impl Into<Option<RequestId>>) -> Self {
        self.request_id = request_id.into();
        self
    }

    /// Exchange order ID the request refers to. A new order carries none - the
    /// exchange assigns one - so this is for [`RequestType::Cancel`] and
    /// [`RequestType::Change`].
    pub fn order_id(mut self, order_id: impl Into<Option<OrderId>>) -> Self {
        self.order_id = order_id.into();
        self
    }

    /// Block the order expires at [default: never].
    pub fn expiry_block(mut self, block: impl Into<Option<u64>>) -> Self {
        self.expiry_block = block.into();
        self
    }

    /// Maximum resting orders this order may match against [default:
    /// unlimited].
    pub fn max_matches(mut self, max_matches: impl Into<Option<u32>>) -> Self {
        self.max_matches = max_matches.into();
        self
    }

    /// Block a [`RequestType::Change`] is conditioned on the order having last
    /// executed at.
    pub fn last_exec_block(mut self, block: impl Into<Option<u64>>) -> Self {
        self.last_exec_block = block.into();
        self
    }

    /// Collateral amount, which carries meaning for
    /// [`RequestType::IncreasePositionCollateral`] rather than a posted order.
    pub fn amount(mut self, amount: impl Into<Option<UD128>>) -> Self {
        self.amount = amount.into();
        self
    }

    /// Reject the order rather than let it take liquidity.
    pub fn post_only(mut self, post_only: bool) -> Self {
        self.post_only = post_only;
        self
    }

    /// Fill the order in full or not at all.
    pub fn fill_or_kill(mut self, fill_or_kill: bool) -> Self {
        self.fill_or_kill = fill_or_kill;
        self
    }

    /// Cancel whatever does not fill immediately.
    pub fn immediate_or_cancel(mut self, immediate_or_cancel: bool) -> Self {
        self.immediate_or_cancel = immediate_or_cancel;
        self
    }

    /// Additional collateral, in basis points of notional, the exchange may
    /// draw to cover the position's negative unrealized PnL on a fill
    /// [default: [`DEFAULT_MAX_NEG_PNL_COLLAT_BPS`]].
    pub fn max_neg_pnl_collat_bps(mut self, bps: u16) -> Self {
        self.max_neg_pnl_collat_bps = bps;
        self
    }

    /// Attributes the order to a builder - see
    /// [`OrderRequest::with_builder`]. Rejected by [`Self::build`] against a
    /// contract that cannot carry attribution.
    pub fn with_builder(mut self, builder: impl Into<Option<BuilderAttribution>>) -> Self {
        self.builder = builder.into();
        self
    }

    /// Quantizes the request against the perpetual's own converters and
    /// validates it against `exchange`.
    pub fn build(self, exchange: &state::Exchange) -> Result<OrderRequest, DexError> {
        let perp = exchange
            .perpetuals()
            .get(&self.perp_id)
            .ok_or(DexError::PerpetualNotTracked(self.perp_id))?;

        // Fail on the states the contract would reject anyway, where the
        // revert reason is far less legible than this
        if exchange.is_halted() {
            return Err(OrderRequestError::ExchangeHalted.into());
        }
        if perp.is_paused() {
            return Err(OrderRequestError::PerpetualPaused(self.perp_id).into());
        }
        if self.post_only && self.fill_or_kill {
            // A post-only order never fills on entry, so there is nothing for
            // fill-or-kill to fill
            return Err(OrderRequestError::ContradictoryFlags("post-only", "fill-or-kill").into());
        }
        if self.builder.is_some() && !exchange.features().builder_attribution() {
            return Err(DexError::UnsupportedByContract(
                "builder attribution",
                exchange.features(),
            ));
        }

        let price = quantize(self.price, perp.price_converter(), OrderField::Price)?;
        let size = quantize(self.size, perp.size_converter(), OrderField::Size)?;
        // Zero is the exchange's "use the maximum" sentinel, so an omitted
        // leverage is spelled out rather than left to resolve silently
        let leverage = match self.leverage {
            Some(leverage) => quantize(leverage, perp.leverage_converter(), OrderField::Leverage)?,
            None => perp.initial_margin(),
        };
        if leverage > perp.initial_margin() {
            return Err(OrderRequestError::LeverageTooHigh {
                perp: self.perp_id,
                requested: leverage,
                max: perp.initial_margin(),
            }
            .into());
        }

        Ok(OrderRequest {
            request_id: self.request_id.unwrap_or_else(default_request_id),
            perp_id: self.perp_id,
            r#type: self.r#type,
            order_id: self.order_id,
            price,
            size,
            expiry_block: self.expiry_block,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            max_matches: self.max_matches,
            leverage,
            last_exec_block: self.last_exec_block,
            amount: self.amount,
            max_neg_pnl_collat_bps: self.max_neg_pnl_collat_bps,
            builder: self.builder,
        })
    }
}

/// Rescales `value` to `converter`'s precision, rejecting anything that would
/// lose digits.
fn quantize(
    value: UD64,
    converter: num::Converter,
    field: OrderField,
) -> Result<UD64, OrderRequestError> {
    let rescaled = value.rescale(converter.decimals() as i16);
    if rescaled != value {
        return Err(OrderRequestError::Precision {
            field,
            value,
            decimals: converter.decimals(),
            rescaled,
        });
    }
    Ok(rescaled)
}

/// Client order ID for a request that did not name one. Milliseconds since the
/// epoch are monotonic enough to keep a session's orders distinguishable in
/// the event stream.
fn default_request_id() -> RequestId {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

impl Display for OrderField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderField::Price => write!(f, "price"),
            OrderField::Size => write!(f, "size"),
            OrderField::Leverage => write!(f, "leverage"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::{primitives::address, sol_types::SolCall};
    use fastnum::{decimal::Context, udec128};

    use super::*;
    use crate::{
        Chain,
        abi::dex,
        num::Converter,
        state::{
            ContractFeatures, ContractVersion, Exchange, FeeSchedule, FeeScheduleKey,
            FeeScheduleRegistry, Perpetual,
        },
    };

    const PERP_ID: PerpetualId = 7;

    fn dec(raw: &str) -> UD64 { UD64::from_str(raw, Context::default()).expect("valid decimal") }

    /// Testnet BTC quotes prices to one decimal place and sizes to five, at up
    /// to 50x, which is enough shape to quantize and bound an order against.
    fn btc() -> Perpetual {
        Perpetual::for_testing(PERP_ID)
            .with_precision(1, 5, 2)
            .with_initial_margin(dec("50"))
    }

    fn exchange_with(perp: Perpetual, features: ContractFeatures, is_halted: bool) -> Exchange {
        Exchange::new(
            Chain::testnet(),
            StateInstant::new(0, 0),
            features,
            Converter::new(6),
            100,
            udec128!(0.001),
            udec128!(0.001),
            udec128!(0.001),
            FeeScheduleRegistry::new(
                FeeSchedule::flat(FeeScheduleKey::Default, UD64::ZERO, UD64::ZERO),
                FeeSchedule::flat(FeeScheduleKey::RwaDefault, UD64::ZERO, UD64::ZERO),
                HashMap::new(),
            ),
            HashMap::from([(perp.id(), perp)]),
            HashMap::new(),
            is_halted,
            true,
        )
    }

    fn exchange() -> Exchange { exchange_with(btc(), ContractFeatures::current(), false) }

    fn builder() -> OrderRequestBuilder {
        OrderRequest::builder(PERP_ID, RequestType::OpenLong, dec("65432.1"), dec("0.001"))
    }

    #[test]
    fn accepts_a_value_that_fits_the_perpetual_precision() {
        let request = builder().build(&exchange()).expect("a valid order");
        assert_eq!(request.price(), dec("65432.1"));
        assert_eq!(request.size(), dec("0.001"));
    }

    #[test]
    fn accepts_a_value_coarser_than_the_perpetual_precision() {
        // A whole-number price is not over-precise, so padding it out to the
        // contract's scale must not read as a loss of digits
        let price = Converter::new(1);
        assert_eq!(quantize(dec("65432"), price, OrderField::Price).unwrap(), dec("65432"));

        let mon = Converter::new(6);
        assert_eq!(quantize(dec("0.05"), mon, OrderField::Price).unwrap(), dec("0.05"));
    }

    #[test]
    fn rejects_a_value_the_perpetual_would_silently_truncate() {
        let err = OrderRequest::builder(
            PERP_ID,
            RequestType::OpenLong,
            dec("65432.123456"),
            dec("0.001"),
        )
        .build(&exchange())
        .expect_err("over-precise price")
        .to_string();
        // The message has to name what the value would have become, or the
        // caller cannot tell how much precision they lost
        assert!(err.contains("65432.1"), "{}", err);
        assert!(err.contains("price"), "{}", err);

        let size = Converter::new(5);
        assert!(quantize(dec("0.0000001"), size, OrderField::Size).is_err());
    }

    #[test]
    fn defaults_leverage_to_the_perpetual_maximum() {
        // Zero is the exchange's "use the maximum" sentinel, so an omitted
        // leverage has to be spelled out rather than left to resolve silently
        let request = builder().build(&exchange()).expect("a valid order");
        assert_eq!(request.leverage(), dec("50"));

        let request = builder()
            .leverage(dec("12.5"))
            .build(&exchange())
            .expect("a valid order");
        assert_eq!(request.leverage(), dec("12.5"));
    }

    #[test]
    fn rejects_leverage_above_the_perpetual_maximum() {
        let err = builder()
            .leverage(dec("100"))
            .build(&exchange())
            .expect_err("leverage above the cap")
            .to_string();
        assert!(err.contains("100"), "{}", err);
        assert!(err.contains("50"), "{}", err);
    }

    #[test]
    fn rejects_contradictory_flags() {
        // A post-only order never fills on entry, so there is nothing for
        // fill-or-kill to fill
        let err = builder()
            .post_only(true)
            .fill_or_kill(true)
            .build(&exchange())
            .expect_err("contradictory flags");
        assert!(matches!(
            err,
            DexError::OrderRequest(OrderRequestError::ContradictoryFlags(
                "post-only",
                "fill-or-kill"
            ))
        ));
    }

    #[test]
    fn rejects_an_order_on_a_halted_exchange_or_paused_perpetual() {
        let halted = exchange_with(btc(), ContractFeatures::current(), true);
        assert!(matches!(
            builder().build(&halted),
            Err(DexError::OrderRequest(OrderRequestError::ExchangeHalted))
        ));

        let paused = exchange_with(btc().with_paused(true), ContractFeatures::current(), false);
        assert!(matches!(
            builder().build(&paused),
            Err(DexError::OrderRequest(OrderRequestError::PerpetualPaused(PERP_ID)))
        ));
    }

    #[test]
    fn rejects_an_order_on_a_perpetual_the_snapshot_does_not_track() {
        let err = OrderRequest::builder(PERP_ID + 1, RequestType::OpenLong, dec("1"), dec("1"))
            .build(&exchange());
        assert!(matches!(err, Err(DexError::PerpetualNotTracked(id)) if id == PERP_ID + 1));
    }

    #[test]
    fn defaults_the_client_order_id_to_the_clock() {
        // Distinguishable orders is the whole point, so two requests built
        // without an ID must not collide, and a named one must survive
        let request = builder().build(&exchange()).expect("a valid order");
        assert!(request.request_id() > 0);
        assert_eq!(
            builder()
                .request_id(4242)
                .build(&exchange())
                .unwrap()
                .request_id(),
            4242
        );
    }

    #[test]
    fn rejects_builder_attribution_a_contract_cannot_carry() {
        let exchange =
            exchange_with(btc(), ContractFeatures::of(ContractVersion::V2_GETTERS), false);
        let err = builder()
            .with_builder(BuilderAttribution::new(7, dec("0.0001")))
            .build(&exchange)
            .expect_err("attribution on a contract without it");
        assert!(matches!(err, DexError::UnsupportedByContract("builder attribution", _)));
    }

    #[test]
    fn posts_through_the_v1_entrypoint_only_where_v2_is_absent() {
        let from = address!("0x0000000000000000000000000000000000000042");

        let v2 = builder()
            .build(&exchange())
            .expect("a valid order")
            .to_transaction_request(&exchange(), from)
            .expect("a transaction");
        assert_eq!(
            v2.input.input().expect("calldata")[..4],
            dex::Exchange::execOrdersV2Call::SELECTOR,
        );
        assert_eq!(v2.to, Some(Chain::testnet().exchange().into()));
        assert_eq!(v2.from, Some(from));

        // A contract that cannot carry an extension envelope has nothing to
        // put one in, so an unattributed order goes through V1
        let legacy = exchange_with(btc(), ContractFeatures::of(ContractVersion::V2_GETTERS), false);
        let v1 = builder()
            .build(&legacy)
            .expect("a valid order")
            .to_transaction_request(&legacy, from)
            .expect("a transaction");
        assert_eq!(
            v1.input.input().expect("calldata")[..4],
            dex::Exchange::execOrdersCall::SELECTOR,
        );
    }
}
