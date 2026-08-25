//! Book order representation with intrusive linked list pointers.

use std::ops::{Deref, DerefMut};

use crate::{state::Order, types};

/// Individual order in the book with linked list pointers.
///
/// Each order belongs to a doubly-linked list at its price level,
/// enabling O(1) insertion/removal and natural FIFO ordering.
#[derive(Clone, Debug)]
pub struct BookOrder {
    order: Order,
    /// Previous order in queue (toward head). None if this is the head.
    prev: Option<types::OrderId>,
    /// Next order in queue (toward tail). None if this is the tail.
    next: Option<types::OrderId>,
}

impl BookOrder {
    /// Create a new book order (initially unlinked).
    pub fn new(order: Order) -> Self { Self { order, prev: None, next: None } }

    /// Previous order in the FIFO queue (toward head).
    pub(crate) fn prev(&self) -> Option<types::OrderId> { self.prev }

    /// Next order in the FIFO queue (toward tail).
    pub(crate) fn next(&self) -> Option<types::OrderId> { self.next }

    /// Update the underlying order data (for size changes).
    pub(crate) fn update_order(&mut self, order: Order) { self.order = order; }

    /// Set the previous order pointer.
    pub(crate) fn set_prev(&mut self, prev: Option<types::OrderId>) { self.prev = prev; }

    /// Set the next order pointer.
    pub(crate) fn set_next(&mut self, next: Option<types::OrderId>) { self.next = next; }
}

impl Deref for BookOrder {
    type Target = Order;

    fn deref(&self) -> &Self::Target { &self.order }
}

impl DerefMut for BookOrder {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.order }
}
