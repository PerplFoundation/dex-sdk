use fastnum::UD64;

use super::{event, types};
use crate::{abi::dex, num};

/// Active order in the perpetual contract order book.
///
/// Exchange order book has a limited capacity of 2^16-1 orders, which requires
/// an extensive reuse of order IDs, up to the point that within the order of execution
/// of a single order request, the same order ID can be used for more than one order.
/// For example, if a taker order partially matches and then gets placed, the matched
/// maker order with order ID = 1 gets removed from the book (and thus vacates the ID),
/// then taker order gets placed under the same order ID = 1.
///
/// So the state of order book and particular mapping between orders and their IDs is
/// tied to a particular point in time and should be used with care.
///
/// Exchange does not support concept of client order IDs and does not store any
/// externally-provided state with orders on-chain, but each order request emits
/// provided [`Order::request_id`] with it, which gets indexed and stored with the order,
/// but with the limitation that this data is available only from events, not
/// with the original snapshot.
///
/// See [`crate::abi::dex::Exchange::OrderDesc`] for more details on particular order parameters
/// and exchange behavior.
/// This wrapper provides automatic conversion from exchnage fixed numeric types to
/// decimal numbers.
///
#[derive(Clone, Copy, derive_more::Debug)]
pub struct Order {
    instant: types::StateInstant,
    request_id: Option<types::RequestId>,
    order_id: types::OrderId,
    r#type: types::OrderType,
    account_id: types::AccountId,
    #[debug("{price}")]
    price: UD64, // SC allocates 24 bits + base price
    #[debug("{size}")]
    size: UD64, // SC allocates 40 bits
    expiry_block: u64,
    #[debug("{leverage}")]
    leverage: UD64,
    post_only: Option<bool>,
    fill_or_kill: Option<bool>,
    immediate_or_cancel: Option<bool>,
    // Linked list pointers for FIFO ordering at each price level.
    // Available from snapshot, None for newly placed orders (until refreshed).
    prev_order_id: Option<types::OrderId>,
    next_order_id: Option<types::OrderId>,
}

impl Order {
    pub(crate) fn new(
        instant: types::StateInstant,
        order: dex::Exchange::Order,
        base_price: UD64,
        price_converter: num::Converter,
        size_converter: num::Converter,
        leverage_converter: num::Converter,
    ) -> Self {
        // Convert 0 to None for linked list pointers (0 means no link)
        let prev_order_id = if order.prevOrderId == 0 {
            None
        } else {
            Some(order.prevOrderId)
        };
        let next_order_id = if order.nextOrderId == 0 {
            None
        } else {
            Some(order.nextOrderId)
        };

        Self {
            instant,
            request_id: None,
            order_id: order.orderId,
            r#type: order.orderType.into(),
            account_id: order.accountId,
            price: base_price + price_converter.from_unsigned(order.priceONS.to()),
            size: size_converter.from_unsigned(order.lotLNS.to()),
            expiry_block: order.expiryBlock as u64,
            leverage: leverage_converter.from_u64(order.leverageHdths as u64),
            post_only: None,
            fill_or_kill: None,
            immediate_or_cancel: None,
            prev_order_id,
            next_order_id,
        }
    }

    pub(crate) fn placed(
        instant: types::StateInstant,
        ctx: &event::OrderContext,
        order_id: types::OrderId,
        size: UD64,
        price_converter: num::Converter,
        leverage_converter: num::Converter,
    ) -> Self {
        Self {
            instant,
            request_id: Some(ctx.request_id),
            order_id,
            r#type: ctx.r#type.into(),
            account_id: ctx.account_id,
            price: price_converter.from_unsigned(ctx.price),
            size,
            expiry_block: ctx.expiry_block,
            leverage: leverage_converter.from_unsigned(ctx.leverage),
            post_only: Some(ctx.post_only),
            fill_or_kill: Some(ctx.fill_or_kill),
            immediate_or_cancel: Some(ctx.immediate_or_cancel),
            // New orders don't have linked list info from events
            prev_order_id: None,
            next_order_id: None,
        }
    }

    pub(crate) fn updated(
        &self,
        instant: types::StateInstant,
        ctx: &Option<event::OrderContext>,
        price: Option<UD64>,
        size: Option<UD64>,
        expiry_block: Option<u64>,
    ) -> Self {
        Self {
            instant,
            request_id: ctx.as_ref().map(|c| c.request_id),
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price: price.unwrap_or(self.price),
            size: size.unwrap_or(self.size),
            expiry_block: expiry_block.unwrap_or(self.expiry_block),
            leverage: self.leverage,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            // Preserve linked list info (may be stale after update, but we maintain
            // ordering separately in BookLevel via sequence numbers)
            prev_order_id: self.prev_order_id,
            next_order_id: self.next_order_id,
        }
    }

    #[allow(unused)]
    pub(crate) fn for_testing(r#type: types::OrderType, price: UD64, size: UD64) -> Self {
        Self {
            instant: types::StateInstant::new(0, 0),
            request_id: None,
            order_id: 0,
            r#type,
            account_id: 0,
            price,
            size,
            expiry_block: 0,
            leverage: UD64::ZERO,
            post_only: None,
            fill_or_kill: None,
            immediate_or_cancel: None,
            prev_order_id: None,
            next_order_id: None,
        }
    }

    /// Create an order for L3 testing with full control over block_number, order_id, account_id.
    #[allow(unused)]
    pub(crate) fn for_l3_testing(
        r#type: types::OrderType,
        price: UD64,
        size: UD64,
        block_number: u64,
        order_id: types::OrderId,
        account_id: types::AccountId,
    ) -> Self {
        Self {
            instant: types::StateInstant::new(block_number, 0),
            request_id: None,
            order_id,
            r#type,
            account_id,
            price,
            size,
            expiry_block: 0,
            leverage: UD64::ZERO,
            post_only: None,
            fill_or_kill: None,
            immediate_or_cancel: None,
            prev_order_id: None,
            next_order_id: None,
        }
    }

    /// Create an order for L3 testing with linked list pointers (for snapshot reconstruction tests).
    #[allow(unused, clippy::too_many_arguments)]
    pub(crate) fn for_l3_testing_with_links(
        r#type: types::OrderType,
        price: UD64,
        size: UD64,
        block_number: u64,
        order_id: types::OrderId,
        account_id: types::AccountId,
        prev_order_id: Option<types::OrderId>,
        next_order_id: Option<types::OrderId>,
    ) -> Self {
        Self {
            instant: types::StateInstant::new(block_number, 0),
            request_id: None,
            order_id,
            r#type,
            account_id,
            price,
            size,
            expiry_block: 0,
            leverage: UD64::ZERO,
            post_only: None,
            fill_or_kill: None,
            immediate_or_cancel: None,
            prev_order_id,
            next_order_id,
        }
    }

    /// Create a copy with updated size (for testing partial fills).
    #[allow(unused)]
    pub(crate) fn with_size(&self, size: UD64) -> Self {
        Self {
            instant: self.instant,
            request_id: self.request_id,
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price: self.price,
            size,
            expiry_block: self.expiry_block,
            leverage: self.leverage,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            prev_order_id: self.prev_order_id,
            next_order_id: self.next_order_id,
        }
    }

    /// Create a copy with updated price (for testing price changes).
    #[allow(unused)]
    pub(crate) fn with_price(&self, price: UD64) -> Self {
        Self {
            instant: self.instant,
            request_id: self.request_id,
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price,
            size: self.size,
            expiry_block: self.expiry_block,
            leverage: self.leverage,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            prev_order_id: self.prev_order_id,
            next_order_id: self.next_order_id,
        }
    }

    /// Create a copy with linked list pointers (for testing snapshot reconstruction).
    #[allow(unused)]
    pub(crate) fn with_links(
        &self,
        prev_order_id: Option<types::OrderId>,
        next_order_id: Option<types::OrderId>,
    ) -> Self {
        Self {
            instant: self.instant,
            request_id: self.request_id,
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price: self.price,
            size: self.size,
            expiry_block: self.expiry_block,
            leverage: self.leverage,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            prev_order_id,
            next_order_id,
        }
    }

    /// Instant the order state is consistent with or was last updated at.
    pub fn instant(&self) -> types::StateInstant {
        self.instant
    }

    /// ID of the request this order was posted by.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn request_id(&self) -> Option<types::RequestId> {
        self.request_id
    }

    /// ID of the order in the book.
    pub fn order_id(&self) -> types::OrderId {
        self.order_id
    }

    /// Type of the order.
    pub fn r#type(&self) -> types::OrderType {
        self.r#type
    }

    /// ID of the account issued the order.
    pub fn account_id(&self) -> types::AccountId {
        self.account_id
    }

    /// Limit price of the order.
    pub fn price(&self) -> UD64 {
        self.price
    }

    /// Size of the order.
    pub fn size(&self) -> UD64 {
        self.size
    }

    /// Expiry block of the order, zero if was not specified.
    pub fn expiry_block(&self) -> u64 {
        self.expiry_block
    }

    /// Leverage of the order.
    pub fn leverage(&self) -> UD64 {
        self.leverage
    }

    /// Post-only flag.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn post_only(&self) -> Option<bool> {
        self.post_only
    }

    /// Fill-or-fill flag.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn fill_or_kill(&self) -> Option<bool> {
        self.fill_or_kill
    }

    /// Immediate-or-cancel flag.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn immediate_or_cancel(&self) -> Option<bool> {
        self.immediate_or_cancel
    }

    /// Previous order ID in the FIFO queue at this price level.
    /// Available from snapshot, None for newly placed orders or if this is the first order.
    pub fn prev_order_id(&self) -> Option<types::OrderId> {
        self.prev_order_id
    }

    /// Next order ID in the FIFO queue at this price level.
    /// Available from snapshot, None for newly placed orders or if this is the last order.
    pub fn next_order_id(&self) -> Option<types::OrderId> {
        self.next_order_id
    }
}
