use std::{
    fmt::Display,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::{
    contract::RawCallBuilder,
    primitives::{Bytes, U256},
    providers::Provider,
};
use fastnum::{UD64, UD128};

use super::*;
use crate::{abi::dex::Exchange::OrderDesc, num, state};

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
    pub fn with_builder_attribution(mut self, builder: BuilderAttribution) -> Self {
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
    pub fn prepare_v2(
        &self,
        exchange: &state::Exchange,
    ) -> Result<(OrderDesc, Bytes), OrderRequestBuilderError> {
        let perp = exchange
            .perpetuals()
            .get(&self.perp_id)
            .ok_or(OrderRequestBuilderError::PerpetualNotTracked(self.perp_id))?;
        let extension = match self.builder {
            None => Bytes::new(),
            Some(builder) => {
                if !exchange.features().builder_attribution() {
                    return Err(OrderRequestBuilderError::UnsupportedByContract(
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

/// Most resting orders the matching engine will walk for a single order
/// (`C._MAX_MATCHES`).
///
/// A loop bound the contract applies for gas safety, not a market parameter:
/// it does not revert above it, it silently substitutes its own maximum, and
/// it does the same for zero. An order that asked to match at most one resting
/// order and got a thousand is the opposite of what was asked, so
/// [`OrderRequestBuilder::build`] rejects anything outside `1..=1000` rather
/// than let the substitution happen.
pub const MAX_MATCHES: u32 = 1000;

/// Field of an order request a fault refers to.
///
/// Carried by [`OrderRequestBuilderError::Precision`] instead of a rendered
/// name, so a caller can name the field in the terms *its* own users typed - a
/// CLI flag, a form field, a JSON key.
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
///
/// Separate from [`DexError`], and deliberately: a client that only reads
/// state never builds a request, and has no business matching on what building
/// one can go wrong with. The three variants that name something the *snapshot*
/// lacks rather than something the caller typed -
/// [`Self::PerpetualNotTracked`], [`Self::OrderNotFound`],
/// [`Self::UnsupportedByContract`] - restate their [`DexError`] counterparts
/// rather than borrowing them, so this enum stands on its own.
#[derive(Clone, Debug, thiserror::Error)]
pub enum OrderRequestBuilderError {
    #[error("account {0} is frozen")]
    AccountFrozen(AccountId),

    #[error("account {0} is not tracked by the snapshot")]
    AccountNotTracked(AccountId),

    #[error("{field} {block} is not ahead of block {at_block}, which the exchange has reached")]
    BlockAlreadyPassed { field: &'static str, block: u64, at_block: u64 },

    #[error("order {0} is a close order, which the exchange does not let a change amend")]
    CannotChangeCloseOrder(OrderId),

    #[error("order {0} has expired, so a change of it has to set a new expiry block")]
    ChangeExpiredOrderNeedsNewExpiry(OrderId),

    #[error("{0} and {1} contradict each other")]
    ContradictoryFlags(&'static str, &'static str),

    #[error("the exchange is halted")]
    ExchangeHalted,

    #[error("leverage {requested} exceeds the maximum of {max} on perpetual {perp}")]
    LeverageTooHigh { perp: PerpetualId, requested: UD64, max: UD64 },

    #[error("max matches {requested} is outside the exchange's range of 1 to {max}")]
    MaxMatchesOutOfRange { requested: u32, max: u32 },

    #[error("a {0:?} request requires {1}")]
    MissingField(RequestType, &'static str),

    #[error("a change of order {0} that amends nothing would spend gas to no effect")]
    NothingToChange(OrderId),

    #[error("order extension error: {0}")]
    OrderExtension(#[from] OrderExtensionError),

    #[error("order {1} not found on perpetual {0}")]
    OrderNotFound(PerpetualId, OrderId),

    #[error("perpetual {0} is not tracked")]
    PerpetualNotTracked(PerpetualId),

    #[error("perpetual {0} is paused")]
    PerpetualPaused(PerpetualId),

    #[error(
        "{field} {value} carries more precision than perpetual's {decimals} decimal place(s) \
         allows; it would become {rescaled}"
    )]
    Precision { field: OrderField, value: UD64, decimals: u8, rescaled: UD64 },

    #[error("deployed exchange contract ({1}) does not support {0}")]
    UnsupportedByContract(&'static str, state::ContractFeatures),
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
        OrderRequestBuilder::new(perp_id, r#type)
            .price(price)
            .size(size)
    }

    /// Cancels a resting order, taking it off the book.
    ///
    /// The price and size the contract wants come from the snapshot's own book
    /// entry for the order, so a caller needs nothing but its ID - see
    /// [`OrderRequestBuilder::build`].
    pub fn cancel(perp_id: PerpetualId, order_id: OrderId) -> OrderRequestBuilder {
        OrderRequestBuilder::new(perp_id, RequestType::Cancel).order_id(order_id)
    }

    /// Amends a resting order: its price level, its size, or its expiry block.
    ///
    /// Cheaper than cancelling and re-posting, which is two operations and
    /// twice the gas. Whatever is left unset keeps the resting order's current
    /// value, so `change(perp, id).price(p)` moves an order and leaves its
    /// size alone. The price is the level the order moves *to*.
    ///
    /// Amending size *down* keeps the order's queue priority; amending it up
    /// sends the order to the back of its level.
    pub fn change(perp_id: PerpetualId, order_id: OrderId) -> OrderRequestBuilder {
        OrderRequestBuilder::new(perp_id, RequestType::Change).order_id(order_id)
    }

    /// Perpetual the request is against.
    pub fn perp_id(&self) -> PerpetualId { self.perp_id }

    /// Client order ID the request is tagged with.
    pub fn request_id(&self) -> RequestId { self.request_id }

    /// What the request asks the exchange to do: post, cancel, change, or top
    /// up a position's collateral.
    pub fn request_type(&self) -> RequestType { self.r#type }

    /// Limit price, in human units and the perpetual's own precision.
    pub fn price(&self) -> UD64 { self.price }

    /// Order size, in human units and the perpetual's own precision.
    pub fn size(&self) -> UD64 { self.size }

    /// Leverage the position is opened at, resolved: a request built by
    /// [`OrderRequestBuilder`] carries the perpetual's maximum where the
    /// caller named none.
    pub fn leverage(&self) -> UD64 { self.leverage }

    /// Exchange ID of the order the request refers to: the order a
    /// [`RequestType::Cancel`] takes off the book or a [`RequestType::Change`]
    /// amends, and `None` for a new order, which the exchange has yet to
    /// assign one.
    pub fn order_id(&self) -> Option<OrderId> { self.order_id }

    /// Last block the exchange may execute this request on, if the caller set
    /// one.
    pub fn last_exec_block(&self) -> Option<u64> { self.last_exec_block }

    /// Block the order stops resting at, if the caller set one. Not to be
    /// confused with [`Self::last_exec_block`], which bounds the *request*
    /// rather than the order it posts.
    pub fn expiry_block(&self) -> Option<u64> { self.expiry_block }

    /// Whether the exchange should reject the order rather than let it take
    /// liquidity.
    pub fn post_only(&self) -> bool { self.post_only }

    /// Whether the order has to fill in full or not at all.
    pub fn fill_or_kill(&self) -> bool { self.fill_or_kill }

    /// Whether whatever does not fill immediately is cancelled rather than
    /// left to rest.
    pub fn immediate_or_cancel(&self) -> bool { self.immediate_or_cancel }

    /// Cap on the resting orders this order may match against, if the caller
    /// set one; see [`MAX_MATCHES`].
    pub fn max_matches(&self) -> Option<u32> { self.max_matches }

    /// This request as a call against `exchange`, ready to simulate or send.
    ///
    /// The sender is left unset for `provider`'s fillers to supply, and
    /// sending needs one of them to carry a wallet that signs for it. What the
    /// SDK hands over is alloy's own builder, so simulating, signing and
    /// waiting on the receipt are done in alloy's vocabulary rather than a
    /// wrapper of ours - see [`crate::exec::orders_call`], which batches.
    pub fn call<P: Provider>(
        &self,
        exchange: &state::Exchange,
        provider: P,
    ) -> Result<RawCallBuilder<P>, OrderRequestBuilderError> {
        crate::exec::orders_call(exchange, provider, std::slice::from_ref(self), true)
    }
}

/// Builds an [`OrderRequest`] from values in human units, checking it against
/// a snapshot before anything is signed.
///
/// The checks are the ones the contract would otherwise apply on chain, where
/// the revert reason is far less legible than an [`OrderRequestBuilderError`] -
/// plus precision, which the contract does not check at all.
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
    // `None` where the resting order the request names is the authority: a
    // cancel needs neither, and a change needs only what it amends
    price: Option<UD64>,
    size: Option<UD64>,
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
    // Not carried into the built request: the exchange takes the account from
    // `msg.sender`, so this only ever feeds the checks in `build`
    account: Option<AccountId>,
}

impl OrderRequestBuilder {
    /// An empty builder for `r#type` on `perp_id`. Reach for
    /// [`OrderRequest::builder`], [`OrderRequest::cancel`] or
    /// [`OrderRequest::change`] instead - each fills in what its request type
    /// needs.
    fn new(perp_id: PerpetualId, r#type: RequestType) -> Self {
        Self {
            perp_id,
            r#type,
            price: None,
            size: None,
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
            account: None,
            builder: None,
        }
    }

    /// Limit price, in human units. On a [`RequestType::Change`] this is the
    /// level the order moves to [default: where it already rests].
    pub fn price(mut self, price: impl Into<Option<UD64>>) -> Self {
        self.price = price.into();
        self
    }

    /// Order size, in human units. On a [`RequestType::Change`] this is the
    /// resting quantity to amend to [default: the size it already has].
    pub fn size(mut self, size: impl Into<Option<UD64>>) -> Self {
        self.size = size.into();
        self
    }

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

    /// Maximum resting orders this order may match against, from 1 to
    /// [`MAX_MATCHES`] [default: [`MAX_MATCHES`], which is what the exchange
    /// walks for an order that names none].
    pub fn max_matches(mut self, max_matches: impl Into<Option<u32>>) -> Self {
        self.max_matches = max_matches.into();
        self
    }

    /// Last block the exchange may execute this request on [default: no
    /// deadline].
    ///
    /// A staleness guard on the *request*, not on the order it names, and it
    /// applies to every request type: past this block the contract rejects the
    /// operation rather than applying it, so a transaction that sat in the
    /// mempool cannot land against a book that has moved on. Not to be
    /// confused with [`Self::expiry_block`], which is how long the order rests
    /// once it is on the book.
    pub fn last_exec_block(mut self, block: impl Into<Option<u64>>) -> Self {
        self.last_exec_block = block.into();
        self
    }

    /// Account the request is for, so the snapshot's own view of it can be
    /// checked before anything is signed [default: unchecked].
    ///
    /// Optional because the request the exchange receives carries no account -
    /// `msg.sender` decides that on chain - so this is purely a pre-flight
    /// check, and a caller that has not asked the snapshot to track the
    /// account has nothing for it to check against.
    pub fn account(mut self, account: impl Into<Option<AccountId>>) -> Self {
        self.account = account.into();
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
    /// [`OrderRequest::with_builder_attribution`]. Rejected by [`Self::build`]
    /// against a contract that cannot carry attribution.
    pub fn builder_attribution(mut self, builder: impl Into<Option<BuilderAttribution>>) -> Self {
        self.builder = builder.into();
        self
    }

    /// Quantizes the request against the perpetual's own converters and
    /// validates it against `exchange`, filling in what the resting order it
    /// names already carries.
    ///
    /// A [`RequestType::Cancel`] or [`RequestType::Change`] is resolved
    /// against the snapshot's book: the order has to be in it, and whatever
    /// the caller left unset - a cancel's price and size, the half of a change
    /// it is not amending - is taken from the order as it rests. That is why
    /// these need a snapshot that tracks the perpetual's book rather than just
    /// its scalers.
    pub fn build(
        self,
        exchange: &state::Exchange,
    ) -> Result<OrderRequest, OrderRequestBuilderError> {
        let perp = exchange
            .perpetuals()
            .get(&self.perp_id)
            .ok_or(OrderRequestBuilderError::PerpetualNotTracked(self.perp_id))?;

        // Cancelling is how a client gets *out*, so it is not blocked on the
        // states that stop an order going on: whether a halted exchange still
        // accepts one is the contract's call, not ours to pre-empt
        if !matches!(self.r#type, RequestType::Cancel) {
            // Fail on the states the contract would reject anyway, where the
            // revert reason is far less legible than this
            if exchange.is_halted() {
                return Err(OrderRequestBuilderError::ExchangeHalted);
            }
            if perp.is_paused() {
                return Err(OrderRequestBuilderError::PerpetualPaused(self.perp_id));
            }
        }
        // A post-only order never fills on entry, so there is nothing for
        // either of the immediate flags to fill
        if self.post_only && self.fill_or_kill {
            return Err(OrderRequestBuilderError::ContradictoryFlags("post-only", "fill-or-kill"));
        }
        if self.post_only && self.immediate_or_cancel {
            return Err(OrderRequestBuilderError::ContradictoryFlags(
                "post-only",
                "immediate-or-cancel",
            ));
        }
        // A block the snapshot has already passed has passed on chain too -
        // the snapshot never runs ahead of the head - so this rejects only
        // what is certainly stale, and lets a deadline the snapshot cannot
        // see yet through to the contract
        let at_block = exchange.instant().block_number();
        if let Some(block) = self.expiry_block.filter(|block| *block <= at_block) {
            return Err(OrderRequestBuilderError::BlockAlreadyPassed {
                field: "expiry block",
                block,
                at_block,
            });
        }
        if let Some(block) = self.last_exec_block.filter(|block| *block <= at_block) {
            return Err(OrderRequestBuilderError::BlockAlreadyPassed {
                field: "last execution block",
                block,
                at_block,
            });
        }
        // Only when the caller named an account, and only against what the
        // snapshot holds: the exchange decides whose order this is from
        // `msg.sender`, so this catches a mistake early rather than deciding
        // anything
        if let Some(account_id) = self.account {
            let account = exchange
                .accounts()
                .get(&account_id)
                .ok_or(OrderRequestBuilderError::AccountNotTracked(account_id))?;
            if account.frozen() {
                return Err(OrderRequestBuilderError::AccountFrozen(account_id));
            }
        }
        // Checked for every request type, including the ones the contract
        // ignores it on: a value it would ignore is a mistake worth hearing
        // about either way
        if let Some(max_matches) = self.max_matches
            && (max_matches == 0 || max_matches > MAX_MATCHES)
        {
            return Err(OrderRequestBuilderError::MaxMatchesOutOfRange {
                requested: max_matches,
                max: MAX_MATCHES,
            });
        }
        if let Some(builder) = self.builder {
            if !exchange.features().builder_attribution() {
                return Err(OrderRequestBuilderError::UnsupportedByContract(
                    "builder attribution",
                    exchange.features(),
                ));
            }
            // The envelope is encoded at `prepare_v2`, which would catch an
            // out-of-range rate on the way to the wire. Catching it here means
            // a request that built is a request that can be sent, and the
            // exchange rejects an over-range rate for the whole batch rather
            // than the one order that carried it
            builder.encode()?;
        }

        // The order a cancel or a change names, which supplies whatever the
        // caller did not
        let resting = match self.r#type {
            RequestType::Cancel | RequestType::Change => {
                let order_id = self
                    .order_id
                    .ok_or(OrderRequestBuilderError::MissingField(self.r#type, "an order ID"))?;
                let order = perp
                    .l3_book()
                    .get_order(order_id)
                    .ok_or(OrderRequestBuilderError::OrderNotFound(self.perp_id, order_id))?;
                if matches!(self.r#type, RequestType::Change) {
                    if matches!(order.r#type(), OrderType::CloseLong | OrderType::CloseShort) {
                        return Err(OrderRequestBuilderError::CannotChangeCloseOrder(order_id));
                    }
                    if self.price.is_none() && self.size.is_none() && self.expiry_block.is_none() {
                        return Err(OrderRequestBuilderError::NothingToChange(order_id));
                    }
                    // An expired order is past the block it was good to, so
                    // the exchange wants to be told the new one explicitly
                    if order.is_expired() && self.expiry_block.is_none() {
                        return Err(OrderRequestBuilderError::ChangeExpiredOrderNeedsNewExpiry(
                            order_id,
                        ));
                    }
                }
                Some(order)
            },
            _ => None,
        };

        // Unreachable through the constructors, each of which supplies what
        // its request type needs - but a missing price is not something to
        // resolve to zero and let the contract puzzle over
        let price = self
            .price
            .or_else(|| resting.map(|order| order.price()))
            .ok_or(OrderRequestBuilderError::MissingField(self.r#type, "a price"))?;
        let size = self
            .size
            .or_else(|| resting.map(|order| order.size()))
            .ok_or(OrderRequestBuilderError::MissingField(self.r#type, "a size"))?;
        let price = quantize(price, perp.price_converter(), OrderField::Price)?;
        let size = quantize(size, perp.size_converter(), OrderField::Size)?;
        let leverage = match self.leverage {
            Some(leverage) => quantize(leverage, perp.leverage_converter(), OrderField::Leverage)?,
            // A cancel or a change describes an order that already carries
            // one. Anything else takes the perpetual's maximum, since zero is
            // the exchange's "use the maximum" sentinel and worth spelling out
            None => resting
                .map(|order| order.leverage())
                .unwrap_or_else(|| perp.initial_margin()),
        };
        // A cancel only takes an order off the book, so the perpetual's current
        // cap is not its business - it was met when the order went on
        if leverage > perp.initial_margin() && !matches!(self.r#type, RequestType::Cancel) {
            return Err(OrderRequestBuilderError::LeverageTooHigh {
                perp: self.perp_id,
                requested: leverage,
                max: perp.initial_margin(),
            });
        }

        Ok(OrderRequest {
            request_id: self.request_id.unwrap_or_else(default_request_id),
            perp_id: self.perp_id,
            r#type: self.r#type,
            order_id: self.order_id,
            price,
            size,
            expiry_block: self.expiry_block.or_else(|| {
                // A change that does not touch the expiry keeps the one the
                // order already has; zero is the contract's "never"
                resting
                    .map(|order| order.expiry_block())
                    .filter(|block| *block > 0)
            }),
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            max_matches: self.max_matches,
            leverage,
            last_exec_block: self.last_exec_block,
            amount: self.amount,
            max_neg_pnl_collat_bps: self.max_neg_pnl_collat_bps,
            // A change rewrites the parameters of an order that already
            // carries its own attribution, and dropping it is the one outcome
            // a builder cannot recover from, so it is carried forward unless
            // the caller named another
            builder: self
                .builder
                .or_else(|| resting.and_then(|order| order.builder())),
        })
    }
}

/// Rescales `value` to `converter`'s precision, rejecting anything that would
/// lose digits.
fn quantize(
    value: UD64,
    converter: num::Converter,
    field: OrderField,
) -> Result<UD64, OrderRequestBuilderError> {
    let rescaled = value.rescale(converter.decimals() as i16);
    if rescaled != value {
        return Err(OrderRequestBuilderError::Precision {
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

    use alloy::{providers::ProviderBuilder, sol_types::SolCall};
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
    fn rejects_max_matches_the_exchange_would_substitute_its_own_for() {
        // The contract does not revert on either bound - it swaps in
        // C._MAX_MATCHES - so an order that asked to walk one resting order
        // would quietly walk a thousand
        for requested in [0, MAX_MATCHES + 1] {
            let err = builder()
                .max_matches(requested)
                .build(&exchange())
                .expect_err("max matches out of range");
            assert!(matches!(
                err,
                OrderRequestBuilderError::MaxMatchesOutOfRange {
                    requested: r,
                    max: MAX_MATCHES,
                } if r == requested
            ));
        }

        // An omitted value is the caller declining to pick one, which is the
        // substitution working as intended
        let request = builder()
            .max_matches(MAX_MATCHES)
            .build(&exchange())
            .expect("a valid order");
        assert_eq!(request.max_matches(), Some(MAX_MATCHES));
        assert_eq!(builder().build(&exchange()).unwrap().max_matches(), None);
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
            OrderRequestBuilderError::ContradictoryFlags("post-only", "fill-or-kill")
        ));
    }

    #[test]
    fn checks_the_account_only_when_the_caller_names_one() {
        // The snapshot the other tests use tracks no accounts at all, so an
        // unnamed account has to stay unchecked rather than fail closed
        builder().build(&exchange()).expect("an unchecked account");
        assert!(matches!(
            builder().account(77).build(&exchange()),
            Err(OrderRequestBuilderError::AccountNotTracked(77))
        ));
    }

    #[test]
    fn rejects_a_deadline_the_exchange_has_already_reached() {
        // The test snapshot sits at block 0, so block 0 is behind it and any
        // later block is still ahead
        for field in ["expiry block", "last execution block"] {
            let with_block = |block| {
                let b = builder();
                if field == "expiry block" {
                    b.expiry_block(block)
                } else {
                    b.last_exec_block(block)
                }
            };
            assert!(matches!(
                with_block(0u64).build(&exchange()),
                Err(OrderRequestBuilderError::BlockAlreadyPassed { field: f, block: 0, at_block: 0 })
                    if f == field
            ));
            with_block(1u64).build(&exchange()).expect("a block ahead");
        }
    }

    #[test]
    fn rejects_post_only_against_either_immediate_flag() {
        // A post-only order never fills on entry, so neither flag has
        // anything to act on
        assert!(matches!(
            builder()
                .post_only(true)
                .immediate_or_cancel(true)
                .build(&exchange()),
            Err(OrderRequestBuilderError::ContradictoryFlags("post-only", "immediate-or-cancel"))
        ));
    }

    #[test]
    fn rejects_a_builder_fee_above_the_contract_ceiling() {
        // Caught here rather than at encoding time, so a request that built is
        // one that can be sent
        let err = builder()
            .builder_attribution(BuilderAttribution::new(7, dec("0.1")))
            .build(&exchange())
            .expect_err("a fee above 1%");
        assert!(matches!(err, OrderRequestBuilderError::OrderExtension(_)), "{}", err);
    }

    #[test]
    fn rejects_an_order_on_a_halted_exchange_or_paused_perpetual() {
        let halted = exchange_with(btc(), ContractFeatures::current(), true);
        assert!(matches!(builder().build(&halted), Err(OrderRequestBuilderError::ExchangeHalted)));

        let paused = exchange_with(btc().with_paused(true), ContractFeatures::current(), false);
        assert!(matches!(
            builder().build(&paused),
            Err(OrderRequestBuilderError::PerpetualPaused(PERP_ID))
        ));
    }

    #[test]
    fn rejects_an_order_on_a_perpetual_the_snapshot_does_not_track() {
        let err = OrderRequest::builder(PERP_ID + 1, RequestType::OpenLong, dec("1"), dec("1"))
            .build(&exchange());
        assert!(
            matches!(err, Err(OrderRequestBuilderError::PerpetualNotTracked(id)) if id == PERP_ID + 1)
        );
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
            .builder_attribution(BuilderAttribution::new(7, dec("0.0001")))
            .build(&exchange)
            .expect_err("attribution on a contract without it");
        assert!(matches!(
            err,
            OrderRequestBuilderError::UnsupportedByContract("builder attribution", _)
        ));
    }

    /// A provider that is never asked for anything: the calldata a builder
    /// carries is settled before any request goes out.
    fn offline_provider() -> impl Provider {
        ProviderBuilder::new().connect_http("http://127.0.0.1:1".parse().expect("a valid url"))
    }

    #[test]
    fn posts_through_the_v1_entrypoint_only_where_v2_is_absent() {
        let v2 = builder()
            .build(&exchange())
            .expect("a valid order")
            .call(&exchange(), offline_provider())
            .expect("a call")
            .into_transaction_request();
        assert_eq!(
            v2.input.input().expect("calldata")[..4],
            dex::Exchange::execOrdersV2Call::SELECTOR,
        );
        assert_eq!(v2.to, Some(Chain::testnet().exchange().into()));
        // The sender is the caller's to fill - a client may sign with a local
        // key, a remote signer or a hardware wallet, and the SDK holds none of
        // them
        assert_eq!(v2.from, None);

        // A contract that cannot carry an extension envelope has nothing to
        // put one in, so an unattributed order goes through V1
        let legacy = exchange_with(btc(), ContractFeatures::of(ContractVersion::V2_GETTERS), false);
        let v1 = builder()
            .build(&legacy)
            .expect("a valid order")
            .call(&legacy, offline_provider())
            .expect("a call")
            .into_transaction_request();
        assert_eq!(
            v1.input.input().expect("calldata")[..4],
            dex::Exchange::execOrdersCall::SELECTOR,
        );
    }

    /// The BTC perpetual with one resting ask of `size` at `price`, which is
    /// order #1 - what a cancel or a change names.
    fn with_resting_ask(price: &str, size: &str) -> Exchange {
        exchange_with(btc().with_ask(dec(price), dec(size)), ContractFeatures::current(), false)
    }

    fn oid(id: u16) -> OrderId { OrderId::new(id).expect("non-zero order id") }

    #[test]
    fn a_cancel_takes_price_and_size_from_the_resting_order() {
        // The caller names nothing but the ID: the book is what knows where
        // the order rests and how much of it is left
        let request = OrderRequest::cancel(PERP_ID, oid(1))
            .build(&with_resting_ask("101000", "0.5"))
            .expect("a valid cancel");
        assert!(matches!(request.request_type(), RequestType::Cancel));
        assert_eq!(request.price(), dec("101000"));
        assert_eq!(request.size(), dec("0.5"));
        assert!(request.request_id() > 0);
    }

    #[test]
    fn a_cancel_is_not_blocked_by_a_halt_or_a_pause() {
        // Cancelling is how a client gets out, so the states that stop an
        // order going on must not stop one coming off
        let halted = exchange_with(
            btc().with_ask(dec("101000"), dec("0.5")),
            ContractFeatures::current(),
            true,
        );
        assert!(OrderRequest::cancel(PERP_ID, oid(1)).build(&halted).is_ok());

        let paused = exchange_with(
            btc().with_ask(dec("101000"), dec("0.5")).with_paused(true),
            ContractFeatures::current(),
            false,
        );
        assert!(OrderRequest::cancel(PERP_ID, oid(1)).build(&paused).is_ok());
        // ... while placing one still is
        assert!(matches!(
            builder().build(&paused),
            Err(OrderRequestBuilderError::PerpetualPaused(PERP_ID))
        ));
    }

    #[test]
    fn a_cancel_or_change_of_an_order_that_is_not_on_the_book_is_rejected() {
        let exchange = with_resting_ask("101000", "0.5");
        assert!(matches!(
            OrderRequest::cancel(PERP_ID, oid(9)).build(&exchange),
            Err(OrderRequestBuilderError::OrderNotFound(PERP_ID, id)) if id == oid(9)
        ));
        assert!(matches!(
            OrderRequest::change(PERP_ID, oid(9)).price(dec("102000")).build(&exchange),
            Err(OrderRequestBuilderError::OrderNotFound(PERP_ID, id)) if id == oid(9)
        ));
    }

    #[test]
    fn a_change_amends_only_what_it_names() {
        let exchange = with_resting_ask("101000", "0.5");

        // Moving the order leaves its size where it was ...
        let moved = OrderRequest::change(PERP_ID, oid(1))
            .price(dec("102000"))
            .build(&exchange)
            .expect("a valid change");
        assert!(matches!(moved.request_type(), RequestType::Change));
        assert_eq!(moved.price(), dec("102000"));
        assert_eq!(moved.size(), dec("0.5"));

        // ... and resizing it leaves the level alone
        let resized = OrderRequest::change(PERP_ID, oid(1))
            .size(dec("0.25"))
            .build(&exchange)
            .expect("a valid change");
        assert_eq!(resized.price(), dec("101000"));
        assert_eq!(resized.size(), dec("0.25"));
    }

    #[test]
    fn a_change_that_amends_nothing_is_rejected() {
        // It would cost gas and change nothing, which is never what was meant
        assert!(matches!(
            OrderRequest::change(PERP_ID, oid(1)).build(&with_resting_ask("101000", "0.5")),
            Err(OrderRequestBuilderError::NothingToChange(id)) if id == oid(1)
        ));
    }

    #[test]
    fn a_change_still_quantizes_what_it_amends() {
        let err = OrderRequest::change(PERP_ID, oid(1))
            .price(dec("102000.123456"))
            .build(&with_resting_ask("101000", "0.5"))
            .expect_err("over-precise price")
            .to_string();
        assert!(err.contains("102000.1"), "{}", err);
    }

    #[test]
    fn a_cancel_or_change_needs_an_order_id() {
        // Reachable only by going around the constructors, which is exactly
        // when a caller needs telling rather than a contract revert
        let err = OrderRequest::builder(PERP_ID, RequestType::Cancel, dec("1"), dec("1"))
            .build(&exchange())
            .expect_err("a cancel without an order");
        assert!(matches!(err, OrderRequestBuilderError::MissingField(RequestType::Cancel, _)));
    }

    #[test]
    fn a_close_order_cannot_be_changed() {
        // The exchange refuses to amend a reduce-only order - it is bound to a
        // position, so its size is not the client's to move
        let exchange = exchange_with(
            btc().with_order(OrderType::CloseLong, dec("101000"), dec("0.5")),
            ContractFeatures::current(),
            false,
        );
        assert!(matches!(
            OrderRequest::change(PERP_ID, oid(1)).price(dec("102000")).build(&exchange),
            Err(OrderRequestBuilderError::CannotChangeCloseOrder(id))
                if id == oid(1)
        ));
        // ... but it can still be cancelled
        assert!(
            OrderRequest::cancel(PERP_ID, oid(1))
                .build(&exchange)
                .is_ok()
        );
    }
}
