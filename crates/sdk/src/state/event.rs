use alloy::primitives::{Address, B256, U256};
use fastnum::{D64, D256, UD64, UD128};

use super::{ContractVersion, FeeSchedule, account, order, perpetual, position};
use crate::{
    abi::dex::Exchange::{OrderRequest, OrderRequestV2, OrderRequestV3},
    types,
};

/// Exchange state processing events.
///
/// This is a subset of [`crate::abi::dex::Exchange::ExchangeEvents`] covering
/// all state mutations and order request error responses handled by SDK,
/// with numeric system conversions applied.
#[derive(Clone, derive_more::Debug)]
pub enum StateEvents {
    /// Account state updated.
    Account(AccountEvent),

    /// Order request processing error.
    Error(OrderError),

    /// Exchange state or configuration updated.
    Exchange(ExchangeEvent),

    /// Order book state updated.
    Order(OrderEvent),

    /// Perpetual contract state or configuration updated.
    Perpetual(PerpetualEvent),

    /// Position state updated.
    Position(PositionEvent),

    /// Trade happened.
    Trade(types::Trade),
}

/// Account state mutation event.
#[derive(Clone, derive_more::Debug)]
pub struct AccountEvent {
    /// ID of the affected account.
    pub account_id: types::AccountId,

    /// ID of the request resulted in this event, if knonw.
    pub request_id: Option<types::RequestId>,

    /// Type of the event with corresponding details.
    pub r#type: AccountEventType,
}

/// Type of account event with corresponding details.
#[derive(Clone, Copy, derive_more::Debug)]
pub enum AccountEventType {
    /// New account created.
    Created(types::AccountId),

    /// Account frozen/unfrozen.
    Frozen(bool),

    /// Account balance updated.
    BalanceUpdated(#[debug("{_0}")] UD128),

    /// Account locked balance updated.
    LockedBalanceUpdated(#[debug("{_0}")] UD128),

    /// Account fee tier updated, taking effect on the account's next fill.
    /// The tier indexes the [`FeeSchedule`] of every contract the account
    /// trades.
    FeeTierUpdated(types::FeeTier),
}

/// Order request processing error with corresponding reason
#[derive(Clone, derive_more::Debug)]
pub struct OrderError {
    /// ID of the perpetual contract of the order.
    pub perpetual_id: types::PerpetualId,

    /// ID of the account issued the order.
    pub account_id: types::AccountId,

    /// ID of the request resulted in this event.
    pub request_id: types::RequestId,

    /// ID of the order the request was targeted at, if known.
    pub order_id: Option<types::OrderId>,

    /// Client order ID involved (contract v1.1.7.6+): the one the failed
    /// request named - to store with the order it would have placed, or
    /// expected of the order it was to cancel or change - or, for a failure
    /// reported on a resting order the request ran into rather than on the
    /// request itself, the one that order stores.
    pub client_order_id: Option<types::ClientOrderId>,

    /// Address that called the exchange with the request (contract v1.1.7.6+,
    /// from `OrderRequestV3`): the account's own address when it sent the
    /// request itself, or the administrator that forwarded the request for
    /// it. An account that is a contract - a delegated account whose operator
    /// sends through it - reports the contract's address, never the
    /// operator's key, so the operator is not told apart here.
    pub sender: Option<Address>,

    /// Failure reason with corresponding details.
    pub r#type: OrderErrorType,
}

/// Type of order request failure with corresponding details.
#[derive(Clone, Copy, derive_more::Debug)]
pub enum OrderErrorType {
    /// Account is frozen.
    AccountFrozen,

    /// Required amount exceeds available balance.
    AmountExceedsAvailableBalance(#[debug("{_0}")] UD128, #[debug("{_1}")] UD128),

    /// Existing close orders mismatch the actual position type and
    /// need to be cancelled before issuing new close orders.
    CancelExistingInvalidCloseOrders,

    /// Close orders can not be changed.
    CantChangeCloseOrder,

    /// Provide new expiration to change expired order.
    ChangeExpiredOrderNeedsNewExpiry,

    /// Close order size exceeds position size.
    CloseOrderExceedsPosition,

    /// Close order side mismatches position type.
    CloseOrderPositionMismatch,

    /// The cancel or change named a client order ID the order does not carry
    /// (contract v1.1.7.6+): the slot holds another order since, or the order
    /// was given another ID, or it was placed without one. `expected` is what
    /// the request named - `None` for a change giving an order its first ID,
    /// which the exchange applies only to an order that has none - and
    /// `stored` what the order carries, `None` for none. Nothing changed.
    ClientOrderIdMismatch {
        expected: Option<types::ClientOrderId>,
        stored: Option<types::ClientOrderId>,
    },

    /// Perpetual contract is not operational.
    ContractNotOperational,

    /// Post-only order crosses the book.
    CrossesBook,

    /// A decrease-collateral request is already pending on the position
    /// (contract v1.1.7.6+): one live request per position, until the pending
    /// one expires at `expiry_timestamp` (seconds since the epoch), is
    /// executed or declined by the position administrator, or is cancelled.
    DecreaseCollateralRequestPending { expiry_timestamp: u64 },

    /// Current block exceeds last execution block specified for the order.
    ExceedsLastExecutionBlock,

    /// Immediate-or-cancel order was not completely filled.
    ImmediateOrCancelExecuted,

    /// Available account balance can not cover recycling fee payment.
    InsuficientFundsForRecycleFee,

    /// Current block exceeds expiration block specified for the order.
    InvalidExpiryBlock,

    /// Specified order ID is out of range.
    InvalidOrderId,

    /// Failed to settle maker order.
    MakerOrderSettlementFailed,

    /// Maximum number of matches reached for the taker order.
    MaxMatchesReached,

    /// Account reached limit of orders to post.
    MaximumAccountOrders,

    /// The perpetual's open interest is at or above the threshold below which
    /// a decrease-collateral request is accepted (contract v1.1.7.6+).
    OpenInterestTooHighForDcp,

    /// Order does not exist.
    OrderDoesNotExist,

    /// Builder-code order extension failed a recoverable decode check (unknown
    /// envelope version, or a builder field out of range) on a batched or
    /// forwarded V2 request, so this single order was skipped while the rest of
    /// the batch proceeded.
    OrderExtensionRejected,

    /// Order posting failed with status.
    OrderPostFailed(u16),

    /// Settlement of the order will render perpetual contract insolvent.
    OrderSettlementImpliesInsolvent,

    /// Size of close order exceeds remaining position size.
    OrderSizeExceedsAvailableSize,

    /// The account has no position on the perpetual to operate on: a
    /// collateral increase, or a decrease-collateral request, without one.
    PositionDoesNotExist,

    /// Order to be posted is under minimum amount.
    PostOrderUnderMinimum,

    /// Specified order price is out of range.
    PriceOutOfRange,

    /// Specified order size is out of range.
    SizeOutOfRange,

    /// A taking order the account sent directly from inside a FastLane
    /// priority-auction bid, refused by the FastLane filter while it is on
    /// (contract v1.1.7.6+). Post-only orders, cancels, changes and the
    /// collateral operations are never refused.
    TakerOrderInFastLaneBid,

    /// Maximum PnL slippage value exceeds maximum of 65535
    ValueExceedsMaximum,

    /// Another account owns the order.
    WrongAccountForOrder,
}

// A `FeeSchedule` carries 16 rates, dwarfing the scalar variants. Boxing it
// would only move an allocation onto a path that is not hot - fee changes are
// rare - at the cost of every consumer's pattern match.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, derive_more::Debug)]
pub enum ExchangeEvent {
    /// Deployed contract version stamped by an upgrade, along with the feature
    /// set it resolves to.
    ContractVersionUpdated(ContractVersion),

    /// A fee schedule in [`super::Exchange::fee_schedules`] was rewritten,
    /// under the key it is registered by ([`FeeSchedule::key`]).
    ///
    /// Every tracked perpetual contract *currently pointing at* that schedule
    /// gets a corresponding [`PerpetualEventType::FeeScheduleUpdated`];
    /// rewriting a schedule never repoints a contract at it, only
    /// `PerpFeeSchedIdSet` does.
    FeeScheduleUpdated(FeeSchedule),

    /// The FastLane taker filter was pointed at a handler, or switched on or
    /// off (contract v1.1.7.6+). While on, a taking order sent directly from
    /// inside a bid on `handler` is refused with
    /// [`OrderErrorType::TakerOrderInFastLaneBid`]. Any administrator key can
    /// switch it, so every one of these is worth matching to an approved
    /// change.
    FastLaneFilterUpdated { handler: Address, enabled: bool },

    /// Exchange halted/unhalted.
    Halted(bool),

    /// Minimal posting amount updated.
    MinPostUpdated(#[debug("{_0}")] UD128),

    /// Minimal settlement amount updated.
    MinSettleUpdated(#[debug("{_0}")] UD128),

    /// Recycling fee updated.
    RecycleFeeUpdated(#[debug("{_0}")] UD128),
}

/// Order book state mutation event.
#[derive(Clone, derive_more::Debug)]
pub struct OrderEvent {
    /// ID of the perpetual contract of the order.
    pub perpetual_id: types::PerpetualId,

    /// ID of the account issued the order.
    pub account_id: types::AccountId,

    /// ID of the request resulted in this event, if knonw.
    pub request_id: Option<types::RequestId>,

    /// ID of the request that placed the order, if known.
    pub placed_request_id: Option<types::RequestId>,

    /// Client order ID the exchange stores with the order, if known - see
    /// [`order::Order::client_order_id`].
    pub client_order_id: Option<types::ClientOrderId>,

    /// ID of the order affected, if knonw.
    pub order_id: Option<types::OrderId>,

    /// Builder the order is attributed to, with the fee rate it charges, if
    /// any.
    ///
    /// Available on contract v1.1.7.4+ for orders whose placement was observed
    /// in the event stream or recovered from the initial snapshot.
    pub builder: Option<types::BuilderAttribution>,

    /// Address that called the exchange with the request this event results
    /// from (contract v1.1.7.6+, from `OrderRequestV3`): the account's own
    /// address when it sent the request itself, or the administrator that
    /// forwarded the request for it. An account that is a contract - a
    /// delegated account whose operator sends through it - reports the
    /// contract's address, never the operator's key, so the operator is not
    /// told apart here. None where the event is not the result of a request,
    /// or predates it.
    pub sender: Option<Address>,

    /// Type of the event with corresponding details.
    pub r#type: OrderEventType,
}

/// Type of order event with corresponding details.
#[derive(Clone, Copy, derive_more::Debug)]
pub enum OrderEventType {
    /// Order filled.
    /// For maker orders this event is paired with [`OrderEventType::Updated`]
    /// or [`OrderEventType::Removed`].
    Filled {
        #[debug("{fill_price}")]
        fill_price: UD64,
        #[debug("{fill_size}")]
        fill_size: UD64,
        /// Total fee the fill charged, builder share included.
        ///
        /// From contract v1.1.7.5 EVERY fill that changes a position's size is
        /// charged, in either direction: a close or decrease pays on the
        /// removed notional at the exit price, netted from the exit
        /// proceeds rather than debited, and an inverting order pays on
        /// its full lot. Earlier releases charged additions only, so a
        /// close fill reports zero there. Liquidation, ADL,
        /// force-close, unwind, frozen-account close and
        /// buy-to-liquidate stay uncharged at any version.
        #[debug("{fee}")]
        fee: UD64, // Precision of SC calculations is limited to 5 decimals.
        /// Portion of `fee` earned by the builder the order is attributed to,
        /// routed entirely to the protocol balance and paid out off-chain.
        ///
        /// Included in `fee`, so consumers must not add it on top. Zero on
        /// liquidation fills and on contracts without builder attribution, and
        /// on close/decrease fills before v1.1.7.5 - a non-zero
        /// [`OrderEvent::builder`] does not imply a non-zero fee here.
        #[debug("{builder_fee}")]
        builder_fee: UD64,
        is_maker: bool,
    },

    /// Order placed to the book.
    Placed {
        r#type: types::OrderType,
        #[debug("{price}")]
        price: UD64,
        #[debug("{size}")]
        size: UD64,
        expiry_block: u64,
        #[debug("{leverage}")]
        leverage: UD64,
        post_only: bool,
        fill_or_kill: bool,
        immediate_or_cancel: bool,
    },

    /// Order removed from the book.
    Removed,

    /// Order in the book updated.
    ///
    /// A change that only gives the order a new client order ID reports that
    /// alone: the order keeps its place in the queue and the collateral it
    /// locks.
    Updated {
        #[debug("{:?}", price.map(|v| format!("{v}")))]
        price: Option<UD64>,
        #[debug("{:?}", size.map(|v| format!("{v}")))]
        size: Option<UD64>,
        expiry_block: Option<u64>,
        /// The client order ID the change gave the order, if it gave one.
        client_order_id: Option<types::ClientOrderId>,
    },
}

/// Perpetual contract state or configuration mutation event.
#[derive(Clone, derive_more::Debug)]
pub struct PerpetualEvent {
    /// ID of the affected perpetual contract.
    pub perpetual_id: types::PerpetualId,

    /// Type of the event with corresponding details.
    pub r#type: PerpetualEventType,
}

/// Type of perpetual event with corresponding details.
// See the note on `ExchangeEvent` about the `FeeSchedule` variant's size.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, derive_more::Debug)]
pub enum PerpetualEventType {
    /// Perpetual contract being added
    Added,

    /// Funding event occured and rate updated.
    FundingEvent {
        #[debug("{rate}")]
        rate: D64,
        #[debug("{payment_per_unit}")]
        payment_per_unit: D256,
    },

    /// Fee schedule of the contract updated, taking effect on its next fill.
    ///
    /// Emitted when the contract's own schedule is rewritten, when it is
    /// repointed at another schedule ([`FeeSchedule::key`] changed), and when
    /// the exchange-wide schedule it points at is rewritten.
    FeeScheduleUpdated(FeeSchedule),

    /// Funding sum scaling exponent updated. The exponent `e` defines the
    /// divider `10^e` applied when interpreting on-chain funding sums and
    /// per-unit funding payments for premium PnL calculations.
    FundingSumScalingExpUpdated(u8),

    /// Initial margin requirement updated.
    InitialMarginFractionUpdated(#[debug("{_0}")] UD64),

    /// Last price updated.
    LastPriceUpdated(#[debug("{_0}")] UD64),

    /// Maintenance margin requirement updated.
    MaintenanceMarginFractionUpdated(#[debug("{_0}")] UD64),

    /// Mark price updated.
    MarkPriceUpdated(#[debug("{_0}")] UD64),

    /// Base (fee tier 0) maker fee updated.
    ///
    /// Deprecated: only replayed from pre-v1.1.7.4 history, where fees were a
    /// single per-contract pair. Current contracts report every fee change as
    /// [`PerpetualEventType::FeeScheduleUpdated`].
    MakerFeeUpdated(#[debug("{_0}")] UD64),

    /// Open interest updated.
    OpenInterestUpdated(#[debug("{_0}")] UD128),

    /// Operating maintenance margin fraction set, or raised along with the
    /// contractual one (contract v1.1.7.6+); `None` when unset. See
    /// [`perpetual::Perpetual::operating_maintenance_margin`]. Every tracked
    /// position on the contract whose requirement moves with it gets a
    /// [`PositionEventType::MaintenanceMarginUpdated`].
    OperatingMaintenanceMarginFractionUpdated(
        #[debug("{:?}", _0.map(|v| format!("{v}")))] Option<UD64>,
    ),

    /// Oracle configuration updated.
    OracleConfigurationUpdated { is_used: bool, feed_id: B256 },

    /// Oracle price updated.
    OraclePriceUpdated(#[debug("{_0}")] UD64),

    /// Perpetual contract paused/unpaused.
    Paused(bool),

    /// Base (fee tier 0) taker fee updated.
    ///
    /// Deprecated, see [`PerpetualEventType::MakerFeeUpdated`].
    TakerFeeUpdated(#[debug("{_0}")] UD64),
}

/// Position state mutation event.
#[derive(Clone, derive_more::Debug)]
pub struct PositionEvent {
    /// ID of the perpetual contract of the position.
    pub perpetual_id: types::PerpetualId,

    /// ID of the account holding the position.
    pub account_id: types::AccountId,

    /// ID of the order request resulted in this event,
    /// if applicable.
    pub request_id: Option<types::RequestId>,

    /// Type of the event with corresponding details.
    pub r#type: PositionEventType,
}

/// Type of position event with corresponding details.
#[derive(Clone, Copy, derive_more::Debug)]
pub enum PositionEventType {
    /// Position closed.
    Closed {
        r#type: position::PositionType,
        #[debug("{entry_price}")]
        entry_price: UD64,
        #[debug("{exit_price}")]
        exit_price: UD64,
        #[debug("{size}")]
        size: UD64,
        #[debug("{delta_pnl}")]
        delta_pnl: D256,
        #[debug("{premium_pnl}")]
        premium_pnl: D256,
    },

    /// Position collateral decreased.
    CollateralDecreased {
        #[debug("{prev_entry_price}")]
        prev_entry_price: UD64,
        #[debug("{new_entry_price}")]
        new_entry_price: UD64,
        #[debug("{deposit}")]
        deposit: UD128,
    },

    /// A request to withdraw `amount` of the position's collateral was filed
    /// (`None` requesting the maximum withdrawable), for the position
    /// administrator to execute or decline before `expiry_timestamp` (seconds
    /// since the epoch). `clamp_to_maximum` lets an amount above the maximum
    /// be clamped to it at execution rather than fail. Filed directly, or
    /// since contract v1.1.7.6 by a
    /// [`types::RequestType::RequestDecreasePositionCollateral`] request,
    /// whose `request_id` it then carries.
    CollateralDecreaseRequested {
        #[debug("{:?}", amount.map(|v| format!("{v}")))]
        amount: Option<UD128>,
        clamp_to_maximum: bool,
        expiry_timestamp: u64,
    },

    /// The pending decrease-collateral request was withdrawn by the account.
    /// Emitted whether or not one was pending.
    CollateralDecreaseRequestCancelled,

    /// The pending decrease-collateral request lapsed unexecuted at
    /// `expiry_timestamp` and was deleted.
    CollateralDecreaseRequestExpired { expiry_timestamp: u64 },

    /// Position decreased.
    Decreased {
        #[debug("{prev_size}")]
        prev_size: UD64,
        #[debug("{new_size}")]
        new_size: UD64,
        #[debug("{deposit}")]
        deposit: UD128,
        #[debug("{delta_pnl}")]
        delta_pnl: D256,
        #[debug("{premium_pnl}")]
        premium_pnl: D256,
    },

    /// Position deleveraged.
    Deleveraged {
        force_close: bool,
        r#type: position::PositionType,
        #[debug("{entry_price}")]
        entry_price: UD64,
        #[debug("{exit_price}")]
        exit_price: UD64,
        #[debug("{prev_size}")]
        prev_size: UD64,
        #[debug("{new_size}")]
        new_size: UD64,
        #[debug("{deposit}")]
        deposit: UD128,
        #[debug("{delta_pnl}")]
        delta_pnl: D256,
        #[debug("{premium_pnl}")]
        premium_pnl: D256,
    },

    /// Position deposit(collateral) updated.
    DepositUpdated(#[debug("{_0}")] UD128),

    /// Position increased.
    Increased {
        #[debug("{entry_price}")]
        entry_price: UD64,
        #[debug("{prev_size}")]
        prev_size: UD64,
        #[debug("{new_size}")]
        new_size: UD64,
        #[debug("{deposit}")]
        deposit: UD128,
    },

    /// Position inverted.
    Inverted {
        r#type: position::PositionType,
        #[debug("{entry_price}")]
        entry_price: UD64,
        #[debug("{prev_size}")]
        prev_size: UD64,
        #[debug("{new_size}")]
        new_size: UD64,
        #[debug("{deposit}")]
        deposit: UD128,
        #[debug("{delta_pnl}")]
        delta_pnl: D256,
        #[debug("{premium_pnl}")]
        premium_pnl: D256,
    },

    /// A liquidation, auto-deleverage, force-close or buy-to-liquidate of the
    /// position left the account's `num_orders` resting orders on this
    /// perpetual in place, because cancelling that many inline could exhaust
    /// the transaction (contract v1.1.7.6+, above 150 of them). No
    /// [`OrderEventType::Removed`] follows: the orders stay on the book, with
    /// the collateral they lock, until they are cancelled, filled or cleared
    /// by matching.
    LiquidationOrderCancelSkipped { num_orders: u32 },

    /// Position liquidated.
    Liquidated {
        r#type: position::PositionType,
        #[debug("{entry_price}")]
        entry_price: UD64,
        #[debug("{exit_price}")]
        exit_price: UD64,
        #[debug("{prev_size}")]
        prev_size: UD64,
        #[debug("{liquidated_size}")]
        liquidated_size: UD64,
        #[debug("{new_size}")]
        new_size: UD64,
        #[debug("{deposit}")]
        deposit: UD128,
        #[debug("{delta_pnl}")]
        delta_pnl: D256,
        #[debug("{premium_pnl}")]
        premium_pnl: D256,
    },

    /// Position maintenance margin requirement updated, because the fraction
    /// it is held to moved - the contractual one, or the operating one where
    /// set ([`perpetual::Perpetual::effective_maintenance_margin`]).
    MaintenanceMarginUpdated(#[debug("{_0}")] UD128),

    /// Position opened.
    Opened {
        r#type: position::PositionType,
        #[debug("{entry_price}")]
        entry_price: UD64,
        #[debug("{size}")]
        size: UD64,
        #[debug("{deposit}")]
        deposit: UD128,
    },

    /// Position unrealized PnL updated.
    UnrealizedPnLUpdated {
        #[debug("{pnl}")]
        pnl: D256,
        #[debug("{delta_pnl}")]
        delta_pnl: D256,
        #[debug("{premium_pnl}")]
        premium_pnl: D256,
    },

    /// Position unwound.
    Unwound {
        r#type: position::PositionType,
        #[debug("{entry_price}")]
        entry_price: UD64,
        #[debug("{exit_price}")]
        exit_price: UD64,
        #[debug("{size}")]
        size: UD64,
        #[debug("{fair_market_value}")]
        fair_market_value: D256,
        #[debug("{payment}")]
        payment: UD128,
    },
}

impl StateEvents {
    pub fn as_account_event(&self) -> Option<AccountEvent> {
        if let StateEvents::Account(account_event) = self {
            Some(account_event.clone())
        } else {
            None
        }
    }

    pub fn as_error(&self) -> Option<OrderError> {
        if let StateEvents::Error(error_event) = self { Some(error_event.clone()) } else { None }
    }

    pub fn as_order_event(&self) -> Option<OrderEvent> {
        if let StateEvents::Order(order_event) = self { Some(order_event.clone()) } else { None }
    }

    pub fn as_exchange_event(&self) -> Option<ExchangeEvent> {
        if let StateEvents::Exchange(exchange_event) = self {
            Some(exchange_event.clone())
        } else {
            None
        }
    }

    pub fn as_perpetual_event(&self) -> Option<PerpetualEvent> {
        if let StateEvents::Perpetual(perpetual_event) = self {
            Some(perpetual_event.clone())
        } else {
            None
        }
    }

    pub fn as_position_event(&self) -> Option<PositionEvent> {
        if let StateEvents::Position(position_event) = self {
            Some(position_event.clone())
        } else {
            None
        }
    }

    pub fn as_trade(&self) -> Option<types::Trade> {
        if let StateEvents::Trade(trade) = self { Some(trade.clone()) } else { None }
    }

    pub(crate) fn account(
        acc: &account::Account,
        ctx: &Option<OrderContext>,
        r#type: AccountEventType,
    ) -> Self {
        Self::Account(AccountEvent {
            account_id: acc.id(),
            request_id: ctx.as_ref().map(|c| c.request_id),
            r#type,
        })
    }

    pub(crate) fn order(
        perp: &perpetual::Perpetual,
        ord: &order::Order,
        ctx: &Option<OrderContext>,
        r#type: OrderEventType,
    ) -> Self {
        Self::Order(OrderEvent {
            perpetual_id: perp.id(),
            account_id: ord.account_id(),
            request_id: ctx.as_ref().map(|c| c.request_id),
            placed_request_id: ord.placed_request_id(),
            client_order_id: ord.client_order_id(),
            order_id: Some(ord.order_id()),
            builder: ord.builder(),
            sender: ctx.as_ref().and_then(|c| c.sender),
            r#type,
        })
    }

    pub(crate) fn order_error(ctx: &OrderContext, r#type: OrderErrorType) -> StateEvents {
        Self::Error(OrderError {
            perpetual_id: ctx.perpetual_id,
            account_id: ctx.account_id,
            request_id: ctx.request_id,
            order_id: ctx.order_id,
            client_order_id: ctx.client_order_id,
            sender: ctx.sender,
            r#type,
        })
    }

    pub(crate) fn affected_order_error(
        ctx: &OrderContext,
        ord: &order::Order,
        r#type: OrderErrorType,
    ) -> StateEvents {
        Self::Error(OrderError {
            perpetual_id: ctx.perpetual_id,
            account_id: ord.account_id(),
            request_id: ctx.request_id,
            order_id: Some(ord.order_id()),
            client_order_id: ord.client_order_id(),
            sender: ctx.sender,
            r#type,
        })
    }

    pub(crate) fn perpetual(
        perp: &perpetual::Perpetual,
        r#type: PerpetualEventType,
    ) -> StateEvents {
        Self::Perpetual(PerpetualEvent { perpetual_id: perp.id(), r#type })
    }

    pub(crate) fn position(
        pos: &position::Position,
        ctx: &Option<OrderContext>,
        r#type: PositionEventType,
    ) -> Self {
        Self::Position(PositionEvent {
            perpetual_id: pos.perpetual_id(),
            account_id: pos.account_id(),
            request_id: ctx.as_ref().map(|c| c.request_id),
            r#type,
        })
    }

    pub(crate) fn trade(ctx: &OrderContext, taker_fee: UD64, taker_builder_fee: UD64) -> Self {
        Self::Trade(types::Trade {
            perpetual_id: ctx.perpetual_id,
            taker_account_id: ctx.account_id,
            taker_request_id: ctx.request_id,
            taker_side: ctx.r#type.try_side().expect("order type with side"),
            taker_fee,
            taker_builder: ctx.builder,
            taker_builder_fee,
            maker_fills: ctx.maker_fills.clone(),
        })
    }
}

/// Order request context.
#[derive(Debug)]
pub(crate) struct OrderContext {
    pub(crate) perpetual_id: types::PerpetualId,
    pub(crate) account_id: types::AccountId,
    pub(crate) request_id: types::RequestId,
    pub(crate) order_id: Option<types::OrderId>,
    pub(crate) r#type: types::RequestType,
    pub(crate) price: U256,
    pub(crate) expiry_block: u64,
    pub(crate) leverage: U256,
    pub(crate) post_only: bool,
    pub(crate) fill_or_kill: bool,
    pub(crate) immediate_or_cancel: bool,
    pub(crate) builder: Option<types::BuilderAttribution>,
    /// The ID named by the request: stored with an order it places, expected
    /// of an order it cancels or changes.
    pub(crate) client_order_id: Option<types::ClientOrderId>,
    /// The ID a change gives the order from then on.
    pub(crate) new_client_order_id: Option<types::ClientOrderId>,
    /// The address that called the exchange - the account's own (a delegated
    /// account's contract address, not its operator's key), or the
    /// administrator that forwarded the order. `OrderRequestV3` only.
    pub(crate) sender: Option<Address>,
    pub(crate) maker_fills: Vec<types::MakerFill>,
    pub(crate) clearing_remaining_order: bool,
    pub(crate) position_closed_at_log_index: Option<u64>,
}

/// The 16 members of the order description, which every version of the
/// order-start event emits under the same names. A macro rather than a
/// function because the three event structs are distinct types.
macro_rules! order_context {
    ($e:expr, $ext:expr, $sender:expr) => {
        OrderContext {
            perpetual_id: types::narrow($e.perpId),
            account_id: types::narrow($e.accountId),
            request_id: types::narrow($e.orderDescId),
            order_id: types::try_order_id($e.orderId),
            r#type: $e.orderType.into(),
            price: $e.pricePNS,
            expiry_block: types::narrow($e.expiryBlock),
            leverage: $e.leverageHdths,
            post_only: $e.postOnly,
            fill_or_kill: $e.fillOrKill,
            immediate_or_cancel: $e.immediateOrCancel,
            builder: $ext.builder(),
            client_order_id: $ext.client_order_id(),
            new_client_order_id: $ext.new_client_order_id(),
            sender: $sender,
            maker_fills: vec![],
            clearing_remaining_order: false,
            position_closed_at_log_index: None,
        }
    };
}

impl From<&OrderRequest> for OrderContext {
    fn from(value: &OrderRequest) -> Self {
        // V1 entrypoints carry no envelope, so no attribution and no IDs
        order_context!(value, types::OrderExtension::default(), None)
    }
}

impl OrderContext {
    /// Context of an `OrderRequestV2` (contract v1.1.7.4 to v1.1.7.5).
    ///
    /// Attribution is not duplicated as event fields: it is recovered by
    /// decoding the raw envelope the request carried, under the rules of the
    /// contract that emitted it - and the event version says which contract
    /// that was. Every contract emitting V2 predates envelope version 2, so a
    /// version-2 envelope here was an unknown version the contract skipped the
    /// order for, and is read as such rather than as the IDs it carries. An
    /// envelope the contract rejected leaves no attribution, and the contract
    /// reports the rejection separately, either by reverting or with
    /// `OrderExtensionRejected`.
    pub(crate) fn from_v2(value: &OrderRequestV2) -> Self {
        let ext = types::OrderExtension::decode_with(&value.extension, false).unwrap_or_default();
        order_context!(value, ext, None)
    }

    /// Context of an `OrderRequestV3` (contract v1.1.7.6+), which adds the
    /// caller. Every contract emitting V3 knows envelope version 2, so the
    /// IDs are read; see [`Self::from_v2`] for the rest.
    pub(crate) fn from_v3(value: &OrderRequestV3) -> Self {
        let ext = types::OrderExtension::decode(&value.extension).unwrap_or_default();
        order_context!(value, ext, Some(value.sender))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `OrderRequest`/`V2` echo the caller's `OrderDesc` before the contract
    /// validates it, and `lastExecutionBlock`/`maxMatches`/`leverageHdths` are
    /// never bounded above, so a full-width field reaches this conversion from
    /// an ordinary account for the price of one transaction. It must saturate,
    /// not panic - a panic takes the consuming application down with it.
    #[test]
    fn order_request_saturates_rather_than_panicking() {
        let ctx = OrderContext::from_v2(&OrderRequestV2 {
            // Bounded on-chain, standing in for the safe half.
            perpId: U256::from(1),
            accountId: U256::from(2),
            orderDescId: U256::MAX,
            orderId: U256::MAX,
            orderType: 1,
            pricePNS: U256::MAX,
            lotLNS: U256::MAX,
            expiryBlock: U256::MAX,
            postOnly: false,
            fillOrKill: false,
            immediateOrCancel: false,
            maxMatches: U256::MAX,
            leverageHdths: U256::MAX,
            lastExecutionBlock: U256::MAX,
            amountCNS: U256::MAX,
            maxNegPnlCollatBPS: U256::MAX,
            gasLeft: U256::MAX,
            extension: Default::default(),
        });

        assert_eq!(ctx.perpetual_id, 1);
        assert_eq!(ctx.account_id, 2);
        assert_eq!(ctx.request_id, u64::MAX);
        assert_eq!(ctx.expiry_block, u64::MAX);
        // Fields the SDK deliberately keeps at full width, and the one it
        // already refused to narrow.
        assert_eq!(ctx.price, U256::MAX);
        assert_eq!(ctx.leverage, U256::MAX);
        assert_eq!(ctx.order_id, None);
    }
}
