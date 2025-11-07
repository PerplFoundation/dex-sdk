use alloy::primitives::{B256, U256};
use fastnum::{D64, D256, UD64, UD128};

use super::{account, order, perpetual, position};

use crate::{abi::dex::Exchange::OrderRequest, types};

/// Exchange state processing events.
///
/// This is a subset of [`crate::abi::dex::Exchange::ExchangeEvents`] covering
/// all state mutations and order request error responses handled by SDK,
/// with numeric system conversions applied.
#[derive(Clone, Debug)]
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
}

/// Account state mutation event.
#[derive(Clone, Debug)]
pub struct AccountEvent {
    /// ID of the affected account.
    pub account_id: types::AccountId,

    /// ID of the request resulted in this event, if knonw.
    pub request_id: Option<types::RequestId>,

    /// Type of the event with corresponding details.
    pub r#type: AccountEventType,
}

/// Type of account event with corresponding details.
#[derive(Clone, Copy, Debug)]
pub enum AccountEventType {
    /// Account frozen/unfrozen.
    Frozen(bool),

    /// Account balance updated.
    BalanceUpdated(UD128),

    /// Account locked balance updated.
    LockedBalanceUpdated(UD128),
}

/// Order request processing error with corresponding reason
#[derive(Clone, Debug)]
pub struct OrderError {
    /// ID of the perpetual contract of the order.
    pub perpetual_id: types::PerpetualId,

    /// ID of the account issued the order.
    pub account_id: types::AccountId,

    /// ID of the request resulted in this event.
    pub request_id: types::RequestId,

    /// ID of the order the request was targeted at, if known.
    pub order_id: Option<types::OrderId>,

    /// Failure reason with corresponding details.
    pub r#type: OrderErrorType,
}

/// Type of order request failure with corresponding details.
#[derive(Clone, Copy, Debug)]
pub enum OrderErrorType {
    /// Account is frozen.
    AccountFrozen,

    /// Required amount exceeds available balance.
    AmountExceedsAvailableBalance(UD128, UD128),

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

    /// Perpetual contract is paused.
    ContractIsPaused,

    /// Post-only order crosses the book.
    CrossesBook,

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

    /// Order does not exist.
    OrderDoesNotExist,

    /// Order posting failed with status.
    OrderPostFailed(u16),

    /// Settlement of the order will render perpetual contract insolvent.
    OrderSettlementImpliesInsolvent,

    /// Size of close order exceeds remaining position size.
    OrderSizeExceedsAvailableSize,

    /// Order to be posted is under minimum amount.
    PostOrderUnderMinimum,

    /// Specified order price is out of range.
    PriceOutOfRange,

    /// Specified order size is out of range.
    SizeOutOfRange,

    /// Another account owns the order.
    WrongAccountForOrder,
}

#[derive(Clone, Debug)]
pub enum ExchangeEvent {
    /// Exchange halted/unhalted.
    Halted(bool),

    /// Minimal posting amount updated.
    MinPostUpdated(UD128),

    /// Minimal settlement amount updated.
    MinSettleUpdated(UD128),

    /// Recycling fee updated.
    RecycleFeeUpdated(UD128),
}

/// Order book state mutation event.
#[derive(Clone, Debug)]
pub struct OrderEvent {
    /// ID of the perpetual contract of the order.
    pub perpetual_id: types::PerpetualId,

    /// ID of the account issued the order.
    pub account_id: types::AccountId,

    /// ID of the request resulted in this event, if knonw.
    pub request_id: Option<types::RequestId>,

    /// ID of the order affected, if knonw.
    pub order_id: Option<types::OrderId>,

    /// Type of the event with corresponding details.
    pub r#type: OrderEventType,
}

/// Type of order event with corresponding details.
#[derive(Clone, Copy, Debug)]
pub enum OrderEventType {
    /// Order filled.
    /// For maker orders this event is paired with [`OrderEventType::Updated`] or
    /// [`OrderEventType::Removed`].
    Filled {
        fill_price: UD64,
        fill_size: UD64,
        fee: UD64, // Precision of SC calculations is limited to 5 decimals.
        is_maker: bool,
    },

    /// Order placed to the book.
    Placed {
        r#type: types::OrderType,
        price: UD64,
        size: UD64,
        expiry_block: u64,
        leverage: UD64,
        post_only: bool,
        fill_or_kill: bool,
        immediate_or_cancel: bool,
    },

    /// Order removed from the book.
    Removed,

    /// Order in the book updated.
    Updated {
        price: Option<UD64>,
        size: Option<UD64>,
        expiry_block: Option<u64>,
    },
}

/// Perpetual contract state or configuration mutation event.
#[derive(Clone, Debug)]
pub struct PerpetualEvent {
    /// ID of the affected perpetual contract.
    pub perpetual_id: types::PerpetualId,

    /// Type of the event with corresponding details.
    pub r#type: PerpetualEventType,
}

/// Type of perpetual event with corresponding details.
#[derive(Clone, Copy, Debug)]
pub enum PerpetualEventType {
    /// Funding event occured and rate updated.
    FundingEvent { rate: D64, payment_per_unit: D256 },

    /// Initial margin requirement updated.
    InitialMarginFractionUpdated(UD64),

    /// Last price updated.
    LastPriceUpdated(UD64),

    /// Maintenance margin requirement updated.
    MaintenanceMarginFractionUpdated(UD64),

    /// Mark price updated.
    MarkPriceUpdated(UD64),

    /// PMaker fee updated.
    MakerFeeUpdated(UD64),

    /// Open interest updated.
    OpenInterestUpdated(UD128),

    /// Oracle configuration updated.
    OracleConfigurationUpdated { is_used: bool, feed_id: B256 },

    /// Oracle price updated.
    OraclePriceUpdated(UD64),

    /// Perpetual contract paused/unpaused.
    Paused(bool),

    /// Taker fee updated.
    TakerFeeUpdated(UD64),
}

/// Position state mutation event.
#[derive(Clone, Debug)]
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
#[derive(Clone, Copy, Debug)]
pub enum PositionEventType {
    // Position closed
    Closed {
        r#type: position::PositionType,
        entry_price: UD64,
        exit_price: UD64,
        size: UD64,
        delta_pnl: D256,
        premium_pnl: D256,
    },

    // Position decreased
    Decreased {
        prev_size: UD64,
        new_size: UD64,
        deposit: UD128,
        delta_pnl: D256,
        premium_pnl: D256,
    },

    // Position deleveraged
    Deleveraged {
        force_close: bool,
        r#type: position::PositionType,
        entry_price: UD64,
        exit_price: UD64,
        prev_size: UD64,
        new_size: UD64,
        deposit: UD128,
        delta_pnl: D256,
        premium_pnl: D256,
    },

    /// Position deposit(collateral) updated.
    DepositUpdated(UD128),

    // Position increased
    Increased {
        entry_price: UD64,
        prev_size: UD64,
        new_size: UD64,
        deposit: UD128,
    },

    // Position inverted
    Inverted {
        r#type: position::PositionType,
        entry_price: UD64,
        prev_size: UD64,
        new_size: UD64,
        deposit: UD128,
        delta_pnl: D256,
        premium_pnl: D256,
    },

    // Position liquidated
    Liquidated {
        r#type: position::PositionType,
        entry_price: UD64,
        exit_price: UD64,
        prev_size: UD64,
        liquidated_size: UD64,
        new_size: UD64,
        deposit: UD128,
        delta_pnl: D256,
        premium_pnl: D256,
    },

    // Position opened
    Opened {
        r#type: position::PositionType,
        entry_price: UD64,
        size: UD64,
        deposit: UD128,
    },

    // Position unrealized PnL updated
    UnrealizedPnLUpdated {
        pnl: D256,
        delta_pnl: D256,
        premium_pnl: D256,
    },

    // Position unwound
    Unwound {
        r#type: position::PositionType,
        entry_price: UD64,
        exit_price: UD64,
        size: UD64,
        fair_market_value: D256,
        payment: UD128,
    },
}

impl StateEvents {
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
            order_id: Some(ord.order_id()),
            r#type,
        })
    }

    pub(crate) fn order_error(ctx: &OrderContext, r#type: OrderErrorType) -> StateEvents {
        Self::Error(OrderError {
            perpetual_id: ctx.perpetual_id,
            account_id: ctx.account_id,
            request_id: ctx.request_id,
            order_id: ctx.order_id,
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
            r#type,
        })
    }

    pub(crate) fn perpetual(
        perp: &perpetual::Perpetual,
        r#type: PerpetualEventType,
    ) -> StateEvents {
        Self::Perpetual(PerpetualEvent {
            perpetual_id: perp.id(),
            r#type,
        })
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
}

/// Order request context.
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
}

impl From<&OrderRequest> for OrderContext {
    fn from(value: &OrderRequest) -> Self {
        let order_id = value.orderId.to::<u16>();
        Self {
            perpetual_id: value.perpId.to(),
            account_id: value.accountId.to(),
            request_id: value.orderDescId.to(),
            order_id: if order_id > 0 { Some(order_id) } else { None },
            r#type: value.orderType.into(),
            price: value.pricePNS,
            expiry_block: value.expiryBlock.to(),
            leverage: value.leverageHdths,
            post_only: value.postOnly,
            fill_or_kill: value.fillOrKill,
            immediate_or_cancel: value.immediateOrCancel,
        }
    }
}
