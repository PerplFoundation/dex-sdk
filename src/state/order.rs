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
/// provided [`request_id`] with it, which gets indexed and stored with the order,
/// but with the limitation that this data is available only from events, not
/// with the original snapshot.
///
/// See [`crate::abi::dex::Exchange::OrderDesc`] for more details on particular order parameters
/// and exchange behavior.
/// This wrapper provides automatic conversion from exchnage fixed numeric types to
/// decimal numbers.
///
#[derive(Clone, Copy, Debug)]
pub struct Order {
    instant: types::StateInstant,
    request_id: Option<types::RequestId>,
    order_id: types::OrderId,
    r#type: types::OrderType,
    account_id: types::AccountId,
    price: UD64, // SC allocates 24 bits + base price
    size: UD64,  // SC allocates 40 bits
    expiry_block: u64,
    leverage: UD64,
    post_only: Option<bool>,
    fill_or_kill: Option<bool>,
    immediate_or_cancel: Option<bool>,
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
}
