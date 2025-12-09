//! L3 order representation with intrusive linked list pointers.

use slotmap::new_key_type;

use crate::{state::Order, types};
use fastnum::UD64;

new_key_type! {
    /// Handle to an order in the slotmap arena.
    pub struct OrderSlot;
}

/// Individual order in the L3 book with linked list pointers.
///
/// Each order belongs to a doubly-linked list at its price level,
/// enabling O(1) insertion/removal and natural FIFO ordering.
#[derive(Clone, Debug)]
pub struct L3Order {
    order: Order,
    /// Previous order in queue (toward head). None if this is the head.
    prev: Option<OrderSlot>,
    /// Next order in queue (toward tail). None if this is the tail.
    next: Option<OrderSlot>,
}

impl L3Order {
    /// Create a new L3 order (initially unlinked).
    pub fn new(order: Order) -> Self {
        Self {
            order,
            prev: None,
            next: None,
        }
    }

    /// The underlying order.
    pub fn order(&self) -> &Order {
        &self.order
    }

    /// Account ID that placed this order.
    pub fn account_id(&self) -> types::AccountId {
        self.order.account_id()
    }

    /// Order ID.
    pub fn order_id(&self) -> types::OrderId {
        self.order.order_id()
    }

    /// Order size.
    pub fn size(&self) -> UD64 {
        self.order.size()
    }

    /// Order price.
    pub fn price(&self) -> UD64 {
        self.order.price()
    }

    /// Order type.
    pub fn r#type(&self) -> types::OrderType {
        self.order.r#type()
    }

    /// Previous order in the FIFO queue (toward head).
    pub(crate) fn prev(&self) -> Option<OrderSlot> {
        self.prev
    }

    /// Next order in the FIFO queue (toward tail).
    pub(crate) fn next(&self) -> Option<OrderSlot> {
        self.next
    }

    /// Update the underlying order data (for size changes).
    pub(crate) fn update_order(&mut self, order: Order) {
        self.order = order;
    }

    /// Set the previous order pointer.
    pub(crate) fn set_prev(&mut self, prev: Option<OrderSlot>) {
        self.prev = prev;
    }

    /// Set the next order pointer.
    pub(crate) fn set_next(&mut self, next: Option<OrderSlot>) {
        self.next = next;
    }
}
