//! L3 price level with linked list of orders.

use fastnum::UD64;

use crate::types;

/// Price level containing orders in a doubly-linked list (FIFO order).
///
/// The level stores head/tail pointers to the linked list and maintains
/// cached aggregates for O(1) access to total size and order count.
#[derive(Clone, Debug, Default)]
pub struct BookLevel {
    /// First order in the FIFO queue (oldest).
    head: Option<types::OrderId>,
    /// Last order in the FIFO queue (newest).
    tail: Option<types::OrderId>,
    /// Cached aggregate: total size at this level.
    cached_size: UD64,
    /// Cached aggregate: number of orders at this level.
    cached_count: u32,
}

impl BookLevel {
    /// Create a new empty book level.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total size at this price level (cached, O(1)).
    pub fn size(&self) -> UD64 {
        self.cached_size
    }

    /// Number of orders at this price level (cached, O(1)).
    pub fn num_orders(&self) -> u32 {
        self.cached_count
    }

    /// First (oldest) order ID at this level.
    pub(crate) fn head(&self) -> Option<types::OrderId> {
        self.head
    }

    /// Last (newest) order ID at this level.
    pub(crate) fn tail(&self) -> Option<types::OrderId> {
        self.tail
    }

    /// Check if this level has no orders.
    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    /// Set the head pointer.
    pub(crate) fn set_head(&mut self, head: Option<types::OrderId>) {
        self.head = head;
    }

    /// Set the tail pointer.
    pub(crate) fn set_tail(&mut self, tail: Option<types::OrderId>) {
        self.tail = tail;
    }

    /// Add to cached size.
    pub(crate) fn add_size(&mut self, size: UD64) {
        self.cached_size += size;
        self.cached_count += 1;
    }

    /// Subtract from cached size.
    pub(crate) fn sub_size(&mut self, size: UD64) {
        self.cached_size -= size;
        self.cached_count -= 1;
    }

    /// Update cached size (for size changes without count change).
    pub(crate) fn update_size(&mut self, old_size: UD64, new_size: UD64) {
        self.cached_size -= old_size;
        self.cached_size += new_size;
    }
}
