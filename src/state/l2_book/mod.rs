//! L2/L3 order book implementation using slotmap arena and intrusive linked lists.
//!
//! This module provides the order book data structure that tracks orders
//! at each price level with FIFO time-priority ordering using doubly-linked lists.
//!
//! # Safety
//!
//! `OrderSlot` handles are internal to each book and must not be used across books.
//! The slot-exposing methods (`all_orders`, `level_orders`) are `pub(crate)` to prevent
//! accidental cross-book slot usage from outside the crate.

mod error;
mod level;
mod order;

#[cfg(test)]
mod tests;

pub use error::{L2BookError, L2BookResult};
pub use level::L3Level;
pub use order::L3Order;
pub(crate) use order::OrderSlot;

use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
};

use fastnum::{UD64, UD128};
use itertools::{FoldWhile, Itertools};
use slotmap::SlotMap;

use crate::{state::Order, types};

/// Slotmap-based L2/L3 order book with intrusive linked lists.
///
/// Orders are stored in a slotmap arena, with each price level maintaining
/// a doubly-linked list of orders in FIFO (time-priority) order.
#[derive(Clone, Debug, Default)]
pub struct L2Book {
    /// Arena storage for all orders.
    orders: SlotMap<OrderSlot, L3Order>,
    /// Reverse index: order_id -> slot for O(1) lookups.
    order_index: HashMap<types::OrderId, OrderSlot>,
    /// Ask levels sorted by price (ascending, best ask first).
    asks: BTreeMap<UD64, L3Level>,
    /// Bid levels sorted by price (descending, best bid first).
    bids: BTreeMap<Reverse<UD64>, L3Level>,
}

impl L2Book {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    // === L2 API ===

    /// Asks sorted away from the spread.
    pub fn asks(&self) -> &BTreeMap<UD64, L3Level> {
        &self.asks
    }

    /// Bids sorted away from the spread.
    pub fn bids(&self) -> &BTreeMap<Reverse<UD64>, L3Level> {
        &self.bids
    }

    /// Best ask price/size.
    pub fn best_ask(&self) -> Option<(UD64, UD64)> {
        self.asks
            .first_key_value()
            .map(|(k, v)| (*k, v.size()))
    }

    /// Best bid price/size.
    pub fn best_bid(&self) -> Option<(UD64, UD64)> {
        self.bids
            .first_key_value()
            .map(|(k, v)| (k.0, v.size()))
    }

    /// Ask impact price for the requested size, along with the fillable size and size-averaged price.
    pub fn ask_impact(&self, want_size: UD64) -> Option<(UD64, UD64, UD64)> {
        Self::impact(self.asks.iter(), want_size)
    }

    /// Bid impact price for the requested size, along with the fillable size and size-averaged price.
    pub fn bid_impact(&self, want_size: UD64) -> Option<(UD64, UD64, UD64)> {
        Self::impact(self.bids.iter().map(|(k, v)| (&k.0, v)), want_size)
    }

    // === L3 API ===

    /// Get L3 level at a specific ask price.
    pub fn ask_level(&self, price: UD64) -> Option<&L3Level> {
        self.asks.get(&price)
    }

    /// Get L3 level at a specific bid price.
    pub fn bid_level(&self, price: UD64) -> Option<&L3Level> {
        self.bids.get(&Reverse(price))
    }

    /// Get a specific order by ID (O(1) via reverse index).
    pub fn get_order(&self, order_id: types::OrderId) -> Option<&L3Order> {
        let slot = self.order_index.get(&order_id)?;
        self.orders.get(*slot)
    }

    /// Get the underlying Order by ID.
    pub fn get_order_data(&self, order_id: types::OrderId) -> Option<&Order> {
        self.get_order(order_id).map(|o| o.order())
    }

    /// Iterator over all L3 orders on the ask side in price-time priority.
    pub fn ask_orders(&self) -> impl Iterator<Item = &L3Order> {
        self.asks.values().flat_map(|level| self.level_orders(level))
    }

    /// Iterator over all L3 orders on the bid side in price-time priority.
    pub fn bid_orders(&self) -> impl Iterator<Item = &L3Order> {
        self.bids.values().flat_map(|level| self.level_orders(level))
    }

    /// Iterator over orders at a specific level (follows the linked list).
    ///
    /// Note: This exposes internal OrderSlot handles. Use within crate only.
    pub(crate) fn level_orders<'a>(&'a self, level: &'a L3Level) -> LevelOrdersIter<'a> {
        LevelOrdersIter {
            orders: &self.orders,
            current: level.head(),
        }
    }

    /// Total number of orders in the book.
    pub fn total_orders(&self) -> usize {
        self.order_index.len()
    }

    /// Access to all orders in the arena.
    ///
    /// Note: This exposes internal OrderSlot handles. Use within crate only.
    pub(crate) fn all_orders(&self) -> &SlotMap<OrderSlot, L3Order> {
        &self.orders
    }

    // === Mutation methods ===

    /// Add an order to the book (at the back of the queue for its price level).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The order already exists in the book
    /// - The order has zero size
    /// - The order has zero price
    pub(crate) fn add_order(&mut self, order: &Order) -> L2BookResult<OrderSlot> {
        // Validate order
        if order.size() == UD64::ZERO {
            return Err(L2BookError::InvalidOrderSize {
                order_id: order.order_id(),
                size: order.size(),
            });
        }
        if order.price() == UD64::ZERO {
            return Err(L2BookError::InvalidOrderPrice {
                order_id: order.order_id(),
                price: order.price(),
            });
        }

        // Check if order already exists
        if let Some(&existing_slot) = self.order_index.get(&order.order_id())
            && let Some(existing) = self.orders.get(existing_slot)
        {
            return Err(L2BookError::OrderAlreadyExists {
                order_id: order.order_id(),
                existing_price: existing.price(),
                existing_slot,
            });
        }

        // Get or create the level
        let side = order.r#type().side();
        let level = match side {
            types::OrderSide::Ask => self.asks.entry(order.price()).or_default(),
            types::OrderSide::Bid => self.bids.entry(Reverse(order.price())).or_default(),
        };

        // Create the L3Order with prev pointing to current tail
        let mut l3_order = L3Order::new(*order);
        l3_order.set_prev(level.tail());

        // Insert into slotmap
        let slot = self.orders.insert(l3_order);

        // Update old tail's next pointer
        if let Some(old_tail) = level.tail()
            && let Some(old_tail_order) = self.orders.get_mut(old_tail)
        {
            old_tail_order.set_next(Some(slot));
        }

        // Update level head/tail
        if level.head().is_none() {
            level.set_head(Some(slot));
        }
        level.set_tail(Some(slot));
        level.add_size(order.size());

        // Update reverse index
        self.order_index.insert(order.order_id(), slot);

        Ok(slot)
    }

    /// Update an order's size (same price level, keeps queue position).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The order doesn't exist in the book
    /// - The new size is zero
    pub(crate) fn update_order(&mut self, order: &Order, _prev_order: &Order) -> L2BookResult<()> {
        // Validate new size
        if order.size() == UD64::ZERO {
            return Err(L2BookError::InvalidOrderSize {
                order_id: order.order_id(),
                size: order.size(),
            });
        }

        // Find the order
        let &slot = self.order_index.get(&order.order_id()).ok_or_else(|| {
            L2BookError::OrderNotFound {
                order_id: order.order_id(),
            }
        })?;

        let l3_order = self.orders.get_mut(slot).ok_or_else(|| {
            L2BookError::OrderNotFound {
                order_id: order.order_id(),
            }
        })?;

        let old_size = l3_order.size();
        let price = l3_order.price();
        let side = l3_order.r#type().side();

        // Update the order data
        l3_order.update_order(*order);

        // Update level cached size
        let level = match side {
            types::OrderSide::Ask => self.asks.get_mut(&price),
            types::OrderSide::Bid => self.bids.get_mut(&Reverse(price)),
        };

        if let Some(level) = level {
            level.update_size(old_size, order.size());
        }

        Ok(())
    }

    /// Remove an order from the book by ID.
    ///
    /// Returns the removed order.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The order doesn't exist in the book
    pub(crate) fn remove_order_by_id(&mut self, order_id: types::OrderId) -> L2BookResult<Order> {
        // Find and remove from index
        let slot = self
            .order_index
            .remove(&order_id)
            .ok_or(L2BookError::OrderNotFound { order_id })?;

        // Get order info before removal
        let l3_order = self
            .orders
            .get(slot)
            .ok_or(L2BookError::OrderNotFound { order_id })?;

        let prev_slot = l3_order.prev();
        let next_slot = l3_order.next();
        let price = l3_order.price();
        let size = l3_order.size();
        let side = l3_order.r#type().side();

        // Update prev's next pointer
        if let Some(prev) = prev_slot
            && let Some(prev_order) = self.orders.get_mut(prev)
        {
            prev_order.set_next(next_slot);
        }

        // Update next's prev pointer
        if let Some(next) = next_slot
            && let Some(next_order) = self.orders.get_mut(next)
        {
            next_order.set_prev(prev_slot);
        }

        // Update level head/tail
        let should_remove_level = match side {
            types::OrderSide::Ask => {
                if let Some(level) = self.asks.get_mut(&price) {
                    if level.head() == Some(slot) {
                        level.set_head(next_slot);
                    }
                    if level.tail() == Some(slot) {
                        level.set_tail(prev_slot);
                    }
                    level.sub_size(size);
                    level.is_empty()
                } else {
                    false
                }
            }
            types::OrderSide::Bid => {
                if let Some(level) = self.bids.get_mut(&Reverse(price)) {
                    if level.head() == Some(slot) {
                        level.set_head(next_slot);
                    }
                    if level.tail() == Some(slot) {
                        level.set_tail(prev_slot);
                    }
                    level.sub_size(size);
                    level.is_empty()
                } else {
                    false
                }
            }
        };

        // Prune empty level
        if should_remove_level {
            match side {
                types::OrderSide::Ask => {
                    self.asks.remove(&price);
                }
                types::OrderSide::Bid => {
                    self.bids.remove(&Reverse(price));
                }
            }
        }

        // Remove from slotmap and return the order
        let removed = self
            .orders
            .remove(slot)
            .ok_or(L2BookError::OrderNotFound { order_id })?;

        Ok(*removed.order())
    }

    /// Move an order to the back of the queue (for size increases).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The order doesn't exist in the book
    pub(crate) fn move_to_back(&mut self, order: &Order, _prev_order: &Order) -> L2BookResult<()> {
        // Find the order
        let &slot = self.order_index.get(&order.order_id()).ok_or_else(|| {
            L2BookError::OrderNotFound {
                order_id: order.order_id(),
            }
        })?;

        let l3_order = self.orders.get(slot).ok_or_else(|| {
            L2BookError::OrderNotFound {
                order_id: order.order_id(),
            }
        })?;

        let prev_slot = l3_order.prev();
        let next_slot = l3_order.next();
        let price = l3_order.price();
        let old_size = l3_order.size();
        let side = l3_order.r#type().side();

        // If already at tail, just update the order data
        let level = match side {
            types::OrderSide::Ask => self.asks.get(&price),
            types::OrderSide::Bid => self.bids.get(&Reverse(price)),
        };

        if level.is_some_and(|l| l.tail() == Some(slot)) {
            // Already at back, just update order data
            if let Some(l3_order) = self.orders.get_mut(slot) {
                l3_order.update_order(*order);
            }
            let level = match side {
                types::OrderSide::Ask => self.asks.get_mut(&price),
                types::OrderSide::Bid => self.bids.get_mut(&Reverse(price)),
            };
            if let Some(level) = level {
                level.update_size(old_size, order.size());
            }
            return Ok(());
        }

        // Unlink from current position
        // Update prev's next
        if let Some(prev) = prev_slot
            && let Some(prev_order) = self.orders.get_mut(prev)
        {
            prev_order.set_next(next_slot);
        }

        // Update next's prev
        if let Some(next) = next_slot
            && let Some(next_order) = self.orders.get_mut(next)
        {
            next_order.set_prev(prev_slot);
        }

        // Update level head if we were the head
        let level = match side {
            types::OrderSide::Ask => self.asks.get_mut(&price),
            types::OrderSide::Bid => self.bids.get_mut(&Reverse(price)),
        };

        if let Some(level) = level {
            if level.head() == Some(slot) {
                level.set_head(next_slot);
            }

            // Link at tail
            let old_tail = level.tail();

            // Update old tail's next
            if let Some(old_tail_slot) = old_tail
                && let Some(old_tail_order) = self.orders.get_mut(old_tail_slot)
            {
                old_tail_order.set_next(Some(slot));
            }

            // Update this order's links and data
            if let Some(l3_order) = self.orders.get_mut(slot) {
                l3_order.set_prev(old_tail);
                l3_order.set_next(None);
                l3_order.update_order(*order);
            }

            // Update level tail and size
            level.set_tail(Some(slot));
            level.update_size(old_size, order.size());
        }

        Ok(())
    }

    /// Add orders from a snapshot, reconstructing FIFO order from linked list pointers.
    ///
    /// Uses the `prev_order_id`/`next_order_id` fields to determine the correct
    /// queue position within each price level.
    ///
    /// # Errors
    ///
    /// Returns an error if any order has invalid size or price.
    pub(crate) fn add_orders_from_snapshot(&mut self, orders: &[Order]) -> L2BookResult<()> {
        // First pass: insert all orders into slotmap and build OrderId -> OrderSlot map
        let mut order_id_to_slot: HashMap<types::OrderId, OrderSlot> = HashMap::new();

        for order in orders {
            if order.size() == UD64::ZERO {
                return Err(L2BookError::InvalidOrderSize {
                    order_id: order.order_id(),
                    size: order.size(),
                });
            }
            if order.price() == UD64::ZERO {
                return Err(L2BookError::InvalidOrderPrice {
                    order_id: order.order_id(),
                    price: order.price(),
                });
            }

            let l3_order = L3Order::new(*order);
            let slot = self.orders.insert(l3_order);
            order_id_to_slot.insert(order.order_id(), slot);
            self.order_index.insert(order.order_id(), slot);
        }

        // Second pass: set prev/next pointers using the order's linked list info
        for order in orders {
            let slot = order_id_to_slot[&order.order_id()];

            // Convert prev_order_id to prev slot
            let prev_slot = order
                .prev_order_id()
                .and_then(|prev_id| order_id_to_slot.get(&prev_id).copied());

            // Convert next_order_id to next slot
            let next_slot = order
                .next_order_id()
                .and_then(|next_id| order_id_to_slot.get(&next_id).copied());

            if let Some(l3_order) = self.orders.get_mut(slot) {
                l3_order.set_prev(prev_slot);
                l3_order.set_next(next_slot);
            }
        }

        // Third pass: build levels with head/tail and cached aggregates
        // Group orders by (price, side)
        let mut level_orders: HashMap<(UD64, types::OrderSide), Vec<OrderSlot>> = HashMap::new();
        for order in orders {
            let slot = order_id_to_slot[&order.order_id()];
            let key = (order.price(), order.r#type().side());
            level_orders.entry(key).or_default().push(slot);
        }

        for ((price, side), slots) in level_orders {
            // Find head (order with no prev in this level)
            let head = slots.iter().find(|&&slot| {
                self.orders
                    .get(slot)
                    .is_some_and(|o| o.prev().is_none_or(|p| !slots.contains(&p)))
            });

            // Find tail (order with no next in this level)
            let tail = slots.iter().find(|&&slot| {
                self.orders
                    .get(slot)
                    .is_some_and(|o| o.next().is_none_or(|n| !slots.contains(&n)))
            });

            // Build level with head/tail and cached aggregates
            let mut level = L3Level::new();
            level.set_head(head.copied());
            level.set_tail(tail.copied());
            for &slot in &slots {
                if let Some(order) = self.orders.get(slot) {
                    level.add_size(order.size());
                }
            }

            match side {
                types::OrderSide::Ask => {
                    self.asks.insert(price, level);
                }
                types::OrderSide::Bid => {
                    self.bids.insert(Reverse(price), level);
                }
            }
        }

        Ok(())
    }

    fn impact<'a>(
        mut side: impl Iterator<Item = (&'a UD64, &'a L3Level)>,
        want_size: UD64,
    ) -> Option<(UD64, UD64, UD64)> {
        let (price, unfilled, price_size) = side
            .fold_while(
                (UD64::ZERO, want_size, UD128::ZERO),
                |(_, unfilled, price_size), (price, level)| {
                    let level_size = level.size();
                    if unfilled > level_size {
                        FoldWhile::Continue((
                            *price,
                            unfilled - level_size,
                            price_size + (price.resize() * level_size.resize()),
                        ))
                    } else {
                        FoldWhile::Done((
                            *price,
                            UD64::ZERO,
                            price_size + (price.resize() * unfilled.resize()),
                        ))
                    }
                },
            )
            .into_inner();
        let filled = want_size - unfilled;
        if filled > UD64::ZERO {
            Some((price, filled, (price_size / filled.resize()).resize()))
        } else {
            None
        }
    }
}

/// Iterator over orders at a price level (follows linked list).
pub(crate) struct LevelOrdersIter<'a> {
    orders: &'a SlotMap<OrderSlot, L3Order>,
    current: Option<OrderSlot>,
}

impl<'a> Iterator for LevelOrdersIter<'a> {
    type Item = &'a L3Order;

    fn next(&mut self) -> Option<Self::Item> {
        let slot = self.current?;
        let order = self.orders.get(slot)?;
        self.current = order.next();
        Some(order)
    }
}
