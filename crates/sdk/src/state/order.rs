use std::num::NonZeroU16;

use fastnum::UD64;
use thiserror::Error;

use super::{event, types};
use crate::{abi::dex, num};

/// Error creating an Order from exchange data.
#[derive(Debug, Clone, Error)]
pub enum OrderParseError {
    /// Order has invalid ID 0 (which is reserved as NULL_ORDER_ID on the
    /// exchange).
    #[error("order has invalid id 0")]
    ZeroOrderId,
}

/// Active order in the perpetual contract order book.
///
/// Exchange order book has a limited capacity of 2^16-1 orders, which requires
/// an extensive reuse of order IDs, up to the point that within the order of
/// execution of a single order request, the same order ID can be used for more
/// than one order. For example, if a taker order partially matches and then
/// gets placed, the matched maker order with order ID = 1 gets removed from the
/// book (and thus vacates the ID), then taker order gets placed under the same
/// order ID = 1.
///
/// So the state of order book and particular mapping between orders and their
/// IDs is tied to a particular point in time and should be used with care.
///
/// Exchange does not support concept of client order IDs and does not store any
/// externally-provided state with orders on-chain, but each order request emits
/// provided [`Order::request_id()`] with it, which gets indexed and stored with
/// the order, with the original request ID preserved as
/// [`Order::client_order_id()`] but with the limitation that this data is
/// available only from events, not from the original snapshot.
///
/// See [`crate::abi::dex::Exchange::OrderDesc`] for more details on particular
/// order parameters and exchange behavior.
/// This wrapper provides automatic conversion from exchnage fixed numeric types
/// to decimal numbers.
#[derive(Clone, Copy, derive_more::Debug)]
pub struct Order {
    instant: types::StateInstant,
    request_id: Option<types::RequestId>,
    client_order_id: Option<types::RequestId>,
    order_id: types::OrderId,
    r#type: types::OrderType,
    account_id: types::AccountId,
    #[debug("{price}")]
    price: UD64, // SC allocates 24 bits + base price
    #[debug("{size}")]
    size: UD64, // SC allocates 40 bits
    placed_size: Option<UD64>, // SC allocates 40 bits
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
    pub(crate) fn from_snapshot(
        instant: types::StateInstant,
        order: dex::Exchange::Order,
        base_price: UD64,
        price_converter: num::Converter,
        size_converter: num::Converter,
        leverage_converter: num::Converter,
    ) -> Result<Self, OrderParseError> {
        // Exchange uses 0 as NULL_ORDER_ID - a valid order must have non-zero ID
        let order_id = NonZeroU16::new(order.orderId).ok_or(OrderParseError::ZeroOrderId)?;

        // Convert 0 to None for linked list pointers (0 means no link)
        // Since we checked orderId != 0 above, NonZeroU16::new() here is safe
        let prev_order_id = NonZeroU16::new(order.prevOrderId);
        let next_order_id = NonZeroU16::new(order.nextOrderId);

        Ok(Self {
            instant,
            request_id: None,
            client_order_id: None, // Not available from snapshot
            order_id,
            r#type: order.orderType.into(),
            account_id: order.accountId,
            price: base_price + price_converter.from_unsigned(order.priceONS.to()),
            size: size_converter.from_unsigned(order.lotLNS.to()),
            placed_size: None,
            expiry_block: order.expiryBlock as u64,
            leverage: leverage_converter.from_u64(order.leverageHdths as u64),
            post_only: None,
            fill_or_kill: None,
            immediate_or_cancel: None,
            prev_order_id,
            next_order_id,
        })
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
            // Original [`request_id`] becomes [`client_order_id`]
            client_order_id: Some(ctx.request_id),
            order_id,
            r#type: ctx.r#type.into(),
            account_id: ctx.account_id,
            price: price_converter.from_unsigned(ctx.price),
            size,
            placed_size: Some(size),
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
        placed_size: Option<UD64>,
        expiry_block: Option<u64>,
    ) -> Self {
        Self {
            instant,
            request_id: ctx.as_ref().map(|c| c.request_id),
            // Original [`client_order_id`] is preserved
            client_order_id: self.client_order_id,
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price: price.unwrap_or(self.price),
            size: size.unwrap_or(self.size),
            placed_size: placed_size.or(self.placed_size),
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

    pub(crate) fn update_if_expired(&mut self, instant: types::StateInstant) -> bool {
        if self.expiry_block != 0
            && self.expiry_block <= instant.block_number()
            && !self.is_expired()
        {
            // Just updating instant so `is_expired` returns true
            self.instant = instant;
            true
        } else {
            false
        }
    }

    #[allow(unused)]
    pub(crate) fn for_testing(r#type: types::OrderType, price: UD64, size: UD64) -> Self {
        Self {
            instant: types::StateInstant::new(0, 0),
            request_id: None,
            client_order_id: None,
            order_id: NonZeroU16::MIN,
            r#type,
            account_id: 0,
            price,
            size,
            placed_size: Some(size),
            expiry_block: 0,
            leverage: UD64::ZERO,
            post_only: None,
            fill_or_kill: None,
            immediate_or_cancel: None,
            prev_order_id: None,
            next_order_id: None,
        }
    }

    /// Create an order for L3 testing with full control over block_number,
    /// order_id, account_id.
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
            client_order_id: None,
            order_id,
            r#type,
            account_id,
            price,
            size,
            placed_size: Some(size),
            expiry_block: 0,
            leverage: UD64::ZERO,
            post_only: None,
            fill_or_kill: None,
            immediate_or_cancel: None,
            prev_order_id: None,
            next_order_id: None,
        }
    }

    /// Create an order for L3 testing with linked list pointers (for snapshot
    /// reconstruction tests).
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
            client_order_id: None,
            order_id,
            r#type,
            account_id,
            price,
            size,
            placed_size: Some(size),
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
            client_order_id: self.client_order_id,
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price: self.price,
            size,
            placed_size: self.placed_size,
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
            client_order_id: self.client_order_id,
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price,
            size: self.size,
            placed_size: self.placed_size,
            expiry_block: self.expiry_block,
            leverage: self.leverage,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            prev_order_id: self.prev_order_id,
            next_order_id: self.next_order_id,
        }
    }

    /// Create a copy with updated expiry block (for testing expiry changes).
    #[allow(unused)]
    pub(crate) fn with_expiry_block(&self, expiry_block: u64) -> Self {
        Self {
            instant: self.instant,
            request_id: self.request_id,
            client_order_id: self.client_order_id,
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price: self.price,
            size: self.size,
            placed_size: self.placed_size,
            expiry_block,
            leverage: self.leverage,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            prev_order_id: self.prev_order_id,
            next_order_id: self.next_order_id,
        }
    }

    /// Create a copy with linked list pointers (for testing snapshot
    /// reconstruction).
    #[allow(unused)]
    pub(crate) fn with_links(
        &self,
        prev_order_id: Option<types::OrderId>,
        next_order_id: Option<types::OrderId>,
    ) -> Self {
        Self {
            instant: self.instant,
            request_id: self.request_id,
            client_order_id: self.client_order_id,
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price: self.price,
            size: self.size,
            placed_size: self.placed_size,
            expiry_block: self.expiry_block,
            leverage: self.leverage,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            prev_order_id,
            next_order_id,
        }
    }

    /// Create a copy with updated client order id.
    #[allow(unused)]
    pub(crate) fn with_client_order_id(&self, client_order_id: types::RequestId) -> Self {
        Self {
            instant: self.instant,
            request_id: self.request_id,
            client_order_id: Some(client_order_id),
            order_id: self.order_id,
            r#type: self.r#type,
            account_id: self.account_id,
            price: self.price,
            size: self.size,
            placed_size: self.placed_size,
            expiry_block: self.expiry_block,
            leverage: self.leverage,
            post_only: self.post_only,
            fill_or_kill: self.fill_or_kill,
            immediate_or_cancel: self.immediate_or_cancel,
            prev_order_id: self.prev_order_id,
            next_order_id: self.next_order_id,
        }
    }

    /// Instant the order state is consistent with or was last updated at.
    pub fn instant(&self) -> types::StateInstant { self.instant }

    /// ID of the request this order was posted or updated by.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn request_id(&self) -> Option<types::RequestId> { self.request_id }

    /// Client order ID = ID of the request this order was placed by.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn client_order_id(&self) -> Option<types::RequestId> { self.client_order_id }

    /// ID of the order in the book.
    pub fn order_id(&self) -> types::OrderId { self.order_id }

    /// Type of the order.
    pub fn r#type(&self) -> types::OrderType { self.r#type }

    /// ID of the account issued the order.
    pub fn account_id(&self) -> types::AccountId { self.account_id }

    /// Limit price of the order.
    pub fn price(&self) -> UD64 { self.price }

    /// Current size of the order.
    pub fn size(&self) -> UD64 { self.size }

    /// Size of the order that was placed.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn placed_size(&self) -> Option<UD64> { self.placed_size }

    /// Filled size of the order.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn filled_size(&self) -> Option<UD64> {
        self.placed_size.map(|placed_size| placed_size - self.size)
    }

    /// Expiry block of the order, zero if was not specified.
    pub fn expiry_block(&self) -> u64 { self.expiry_block }

    /// Check if the order is expired.
    /// NOTE: Valid only after the end of expiry block processing.
    pub fn is_expired(&self) -> bool {
        self.expiry_block != 0 && self.expiry_block <= self.instant.block_number()
    }

    /// Leverage of the order.
    pub fn leverage(&self) -> UD64 { self.leverage }

    /// Post-only flag.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn post_only(&self) -> Option<bool> { self.post_only }

    /// Fill-or-fill flag.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn fill_or_kill(&self) -> Option<bool> { self.fill_or_kill }

    /// Immediate-or-cancel flag.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn immediate_or_cancel(&self) -> Option<bool> { self.immediate_or_cancel }

    /// Previous order ID in the FIFO queue at this price level.
    /// Available from snapshot, None for newly placed orders or if this is the
    /// first order.
    pub fn prev_order_id(&self) -> Option<types::OrderId> { self.prev_order_id }

    /// Next order ID in the FIFO queue at this price level.
    /// Available from snapshot, None for newly placed orders or if this is the
    /// last order.
    pub fn next_order_id(&self) -> Option<types::OrderId> { self.next_order_id }
}

impl std::fmt::Display for Order {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            // Short order representation
            let mut short = format!(
                "{} {:#} #{} 👤{}",
                self.size(),
                self.r#type(),
                self.order_id(),
                self.account_id(),
            );
            if self.expiry_block > 0 {
                short.push_str(format!(" ⏳{}", self.expiry_block).as_str());
            }
            write!(f, "[{}]", short)
        } else {
            write!(
                f,
                "[{}@{} {:#} #{} acc:{} rq:{} exp:{}{} lev:{}]",
                self.size(),
                self.price(),
                self.r#type(),
                self.order_id(),
                self.account_id(),
                self.request_id().unwrap_or_default(),
                self.expiry_block(),
                if self.is_expired() { " (expired)" } else { "" },
                self.leverage(),
            )
        }
    }
}

#[cfg(feature = "display")]
impl tabled::Tabled for Order {
    const LENGTH: usize = 11;

    fn fields(&self) -> Vec<std::borrow::Cow<'_, str>> {
        use colored::Colorize;

        use crate::types::OrderSide;

        vec![
            match self.r#type.side() {
                OrderSide::Ask => self.price().to_string().red().to_string().into(),
                OrderSide::Bid => self.price().to_string().green().to_string().into(),
            },
            match self.r#type.side() {
                OrderSide::Ask => self.size().to_string().red().to_string().into(),
                OrderSide::Bid => self.size().to_string().green().to_string().into(),
            },
            match self.r#type.side() {
                OrderSide::Ask => self.r#type().to_string().red().to_string().into(),
                OrderSide::Bid => self.r#type().to_string().green().to_string().into(),
            },
            self.order_id().to_string().into(),
            self.account_id().to_string().into(),
            if let Some(request_id) = self.request_id() {
                request_id.to_string().into()
            } else {
                "-".to_string().into()
            },
            if let Some(client_order_id) = self.client_order_id() {
                client_order_id.to_string().into()
            } else {
                "-".to_string().into()
            },
            if self.expiry_block() > 0 {
                if self.is_expired() {
                    (self.expiry_block().to_string() + " (expired)")
                        .bright_red()
                        .to_string()
                        .into()
                } else {
                    self.expiry_block().to_string().into()
                }
            } else {
                "-".to_string().into()
            },
            self.leverage().to_string().into(),
            if self.post_only.unwrap_or_default() { "+" } else { "" }
                .to_string()
                .into(),
            if self.fill_or_kill.unwrap_or_default() { "+" } else { "" }
                .to_string()
                .into(),
            if self.immediate_or_cancel.unwrap_or_default() { "+" } else { "" }
                .to_string()
                .into(),
        ]
    }

    fn headers() -> Vec<std::borrow::Cow<'static, str>> {
        vec![
            "Price".into(),
            "Size".into(),
            "Type".into(),
            "Order ID".into(),
            "Account ID".into(),
            "Request ID".into(),
            "Client Order ID".into(),
            "Expiry Block".into(),
            "Leverage".into(),
            "PO".into(),
            "FoK".into(),
            "IoC".into(),
        ]
    }
}
