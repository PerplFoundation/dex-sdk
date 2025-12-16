//! Order book implementation using slotmap arena and intrusive linked lists.
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

pub use error::{OrderBookError, OrderBookResult};
pub use level::BookLevel;
pub use order::BookOrder;
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
pub struct OrderBook {
    /// Arena storage for all orders.
    orders: SlotMap<OrderSlot, BookOrder>,
    /// Reverse index: order_id -> slot for O(1) lookups.
    order_index: HashMap<types::OrderId, OrderSlot>,
    /// Ask levels sorted by price (ascending, best ask first).
    asks: BTreeMap<UD64, BookLevel>,
    /// Bid levels sorted by price (descending, best bid first).
    bids: BTreeMap<Reverse<UD64>, BookLevel>,
}

impl OrderBook {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    // === L2 API ===

    /// Asks sorted away from the spread.
    pub fn asks(&self) -> &BTreeMap<UD64, BookLevel> {
        &self.asks
    }

    /// Bids sorted away from the spread.
    pub fn bids(&self) -> &BTreeMap<Reverse<UD64>, BookLevel> {
        &self.bids
    }

    /// Best ask price/size.
    pub fn best_ask(&self) -> Option<(UD64, UD64)> {
        self.asks.first_key_value().map(|(k, v)| (*k, v.size()))
    }

    /// Best bid price/size.
    pub fn best_bid(&self) -> Option<(UD64, UD64)> {
        self.bids.first_key_value().map(|(k, v)| (k.0, v.size()))
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
    pub fn ask_level(&self, price: UD64) -> Option<&BookLevel> {
        self.asks.get(&price)
    }

    /// Get L3 level at a specific bid price.
    pub fn bid_level(&self, price: UD64) -> Option<&BookLevel> {
        self.bids.get(&Reverse(price))
    }

    /// Get a specific order by ID (O(1) via reverse index).
    pub fn get_order(&self, order_id: types::OrderId) -> Option<&BookOrder> {
        let slot = self.order_index.get(&order_id)?;
        self.orders.get(*slot)
    }

    /// Get the underlying Order by ID.
    pub fn get_order_data(&self, order_id: types::OrderId) -> Option<&Order> {
        self.get_order(order_id).map(|o| o.order())
    }

    /// Iterator over all L3 orders on the ask side in price-time priority.
    pub fn ask_orders(&self) -> impl Iterator<Item = &BookOrder> {
        self.asks
            .values()
            .flat_map(|level| self.level_orders(level))
    }

    /// Iterator over all L3 orders on the bid side in price-time priority.
    pub fn bid_orders(&self) -> impl Iterator<Item = &BookOrder> {
        self.bids
            .values()
            .flat_map(|level| self.level_orders(level))
    }

    /// Iterator over orders at a specific level (follows the linked list).
    ///
    /// Note: This exposes internal OrderSlot handles. Use within crate only.
    pub(crate) fn level_orders<'a>(&'a self, level: &'a BookLevel) -> LevelOrdersIter<'a> {
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
    pub(crate) fn all_orders(&self) -> &SlotMap<OrderSlot, BookOrder> {
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
    pub(crate) fn add_order(&mut self, order: &Order) -> OrderBookResult<OrderSlot> {
        // Validate order
        if order.size() == UD64::ZERO {
            return Err(OrderBookError::InvalidOrderSize {
                order_id: order.order_id(),
                size: order.size(),
            });
        }
        if order.price() == UD64::ZERO {
            return Err(OrderBookError::InvalidOrderPrice {
                order_id: order.order_id(),
                price: order.price(),
            });
        }

        // Check if order already exists
        if let Some(&existing_slot) = self.order_index.get(&order.order_id())
            && let Some(existing) = self.orders.get(existing_slot)
        {
            return Err(OrderBookError::OrderAlreadyExists {
                order_id: order.order_id(),
                existing_price: existing.price(),
                existing_slot,
            });
        }

        // Get or create the level and capture tail before inserting
        let side = order.r#type().side();
        let old_tail = self.get_or_create_level_mut(side, order.price()).tail();

        // Create the BookOrder with prev pointing to current tail
        let mut l3_order = BookOrder::new(*order);
        l3_order.set_prev(old_tail);

        // Insert into slotmap
        let slot = self.orders.insert(l3_order);

        // Link at tail
        self.link_at_tail(side, order.price(), old_tail, slot, order.size());

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
    pub(crate) fn update_order(
        &mut self,
        order: &Order,
        _prev_order: &Order,
    ) -> OrderBookResult<()> {
        // Validate new size
        if order.size() == UD64::ZERO {
            return Err(OrderBookError::InvalidOrderSize {
                order_id: order.order_id(),
                size: order.size(),
            });
        }

        // Find the order
        let &slot = self.order_index.get(&order.order_id()).ok_or_else(|| {
            OrderBookError::OrderNotFound {
                order_id: order.order_id(),
            }
        })?;

        let l3_order = self
            .orders
            .get_mut(slot)
            .ok_or_else(|| OrderBookError::OrderNotFound {
                order_id: order.order_id(),
            })?;

        let old_size = l3_order.size();
        let price = l3_order.price();
        let side = l3_order.r#type().side();

        // Update the order data
        l3_order.update_order(*order);

        // Update level cached size
        let level = self
            .get_level_mut(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?;
        level.update_size(old_size, order.size());

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
    pub(crate) fn remove_order_by_id(
        &mut self,
        order_id: types::OrderId,
    ) -> OrderBookResult<Order> {
        // Find and remove from index
        let slot = self
            .order_index
            .remove(&order_id)
            .ok_or(OrderBookError::OrderNotFound { order_id })?;

        // Get order info before removal
        let l3_order = self
            .orders
            .get(slot)
            .ok_or(OrderBookError::OrderNotFound { order_id })?;

        let prev_slot = l3_order.prev();
        let next_slot = l3_order.next();
        let price = l3_order.price();
        let size = l3_order.size();
        let side = l3_order.r#type().side();

        // Unlink from list
        self.unlink_node(prev_slot, next_slot);

        // Update level head/tail and check if empty
        let level = self
            .get_level_mut(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?;
        if level.head() == Some(slot) {
            level.set_head(next_slot);
        }
        if level.tail() == Some(slot) {
            level.set_tail(prev_slot);
        }
        level.sub_size(size);
        let should_remove_level = level.is_empty();

        // Prune empty level
        if should_remove_level {
            self.remove_level(side, price);
        }

        // Remove from slotmap and return the order
        let removed = self
            .orders
            .remove(slot)
            .ok_or(OrderBookError::OrderNotFound { order_id })?;

        Ok(*removed.order())
    }

    /// Move an order to the back of the queue (for size increases).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The order doesn't exist in the book
    pub(crate) fn move_to_back(
        &mut self,
        order: &Order,
        _prev_order: &Order,
    ) -> OrderBookResult<()> {
        // Find the order
        let &slot = self.order_index.get(&order.order_id()).ok_or_else(|| {
            OrderBookError::OrderNotFound {
                order_id: order.order_id(),
            }
        })?;

        let l3_order = self
            .orders
            .get(slot)
            .ok_or_else(|| OrderBookError::OrderNotFound {
                order_id: order.order_id(),
            })?;

        let prev_slot = l3_order.prev();
        let next_slot = l3_order.next();
        let price = l3_order.price();
        let old_size = l3_order.size();
        let side = l3_order.r#type().side();

        // If already at tail, just update the order data
        let is_at_tail = self
            .get_level(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?
            .tail()
            == Some(slot);

        if is_at_tail {
            // Already at back, just update order data
            if let Some(l3_order) = self.orders.get_mut(slot) {
                l3_order.update_order(*order);
            }
            let level = self
                .get_level_mut(side, price)
                .ok_or(OrderBookError::LevelNotFound { price, side })?;
            level.update_size(old_size, order.size());
            return Ok(());
        }

        // Unlink from current position
        self.unlink_node(prev_slot, next_slot);

        // Update level head if we were the head
        let level = self
            .get_level_mut(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?;
        if level.head() == Some(slot) {
            level.set_head(next_slot);
        }

        // Get old tail before updating
        let old_tail = level.tail();

        // Update old tail's next pointer
        if let Some(old_tail_slot) = old_tail {
            if let Some(old_tail_order) = self.orders.get_mut(old_tail_slot) {
                old_tail_order.set_next(Some(slot));
            }
        }

        // Update this order's links and data
        if let Some(l3_order) = self.orders.get_mut(slot) {
            l3_order.set_prev(old_tail);
            l3_order.set_next(None);
            l3_order.update_order(*order);
        }

        // Update level tail and size - need to re-borrow
        let level = self
            .get_level_mut(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?;
        level.set_tail(Some(slot));
        level.update_size(old_size, order.size());

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
    pub(crate) fn add_orders_from_snapshot(&mut self, orders: &[Order]) -> OrderBookResult<()> {
        // First pass: insert all orders into slotmap and build OrderId -> OrderSlot map
        let mut order_id_to_slot: HashMap<types::OrderId, OrderSlot> = HashMap::new();

        for order in orders {
            if order.size() == UD64::ZERO {
                return Err(OrderBookError::InvalidOrderSize {
                    order_id: order.order_id(),
                    size: order.size(),
                });
            }
            if order.price() == UD64::ZERO {
                return Err(OrderBookError::InvalidOrderPrice {
                    order_id: order.order_id(),
                    price: order.price(),
                });
            }

            let l3_order = BookOrder::new(*order);
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
            let mut level = BookLevel::new();
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

    // === Linked list helpers ===

    /// Get a level by side and price (immutable).
    fn get_level(&self, side: types::OrderSide, price: UD64) -> Option<&BookLevel> {
        match side {
            types::OrderSide::Ask => self.asks.get(&price),
            types::OrderSide::Bid => self.bids.get(&Reverse(price)),
        }
    }

    /// Get a level by side and price (mutable).
    fn get_level_mut(&mut self, side: types::OrderSide, price: UD64) -> Option<&mut BookLevel> {
        match side {
            types::OrderSide::Ask => self.asks.get_mut(&price),
            types::OrderSide::Bid => self.bids.get_mut(&Reverse(price)),
        }
    }

    /// Get or create a level by side and price.
    fn get_or_create_level_mut(&mut self, side: types::OrderSide, price: UD64) -> &mut BookLevel {
        match side {
            types::OrderSide::Ask => self.asks.entry(price).or_default(),
            types::OrderSide::Bid => self.bids.entry(Reverse(price)).or_default(),
        }
    }

    /// Remove a level by side and price.
    fn remove_level(&mut self, side: types::OrderSide, price: UD64) {
        match side {
            types::OrderSide::Ask => {
                self.asks.remove(&price);
            }
            types::OrderSide::Bid => {
                self.bids.remove(&Reverse(price));
            }
        }
    }

    /// Force remove a level (for testing state inconsistency handling).
    #[cfg(test)]
    pub(crate) fn force_remove_level(&mut self, side: types::OrderSide, price: UD64) {
        self.remove_level(side, price);
    }

    /// Unlink a node from the doubly-linked list by updating its neighbors.
    fn unlink_node(&mut self, prev_slot: Option<OrderSlot>, next_slot: Option<OrderSlot>) {
        // Update prev's next pointer
        if let Some(prev) = prev_slot {
            if let Some(prev_order) = self.orders.get_mut(prev) {
                prev_order.set_next(next_slot);
            }
        }

        // Update next's prev pointer
        if let Some(next) = next_slot {
            if let Some(next_order) = self.orders.get_mut(next) {
                next_order.set_prev(prev_slot);
            }
        }
    }

    /// Link a new slot at the tail of a level.
    /// Takes old_tail to avoid borrowing level while mutating orders.
    fn link_at_tail(
        &mut self,
        side: types::OrderSide,
        price: UD64,
        old_tail: Option<OrderSlot>,
        slot: OrderSlot,
        size: UD64,
    ) {
        // Update old tail's next pointer
        if let Some(old_tail_slot) = old_tail {
            if let Some(old_tail_order) = self.orders.get_mut(old_tail_slot) {
                old_tail_order.set_next(Some(slot));
            }
        }

        // Update level head/tail (re-borrow level after updating orders)
        let level = self.get_or_create_level_mut(side, price);
        if level.head().is_none() {
            level.set_head(Some(slot));
        }
        level.set_tail(Some(slot));
        level.add_size(size);
    }

    fn impact<'a>(
        mut side: impl Iterator<Item = (&'a UD64, &'a BookLevel)>,
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
    orders: &'a SlotMap<OrderSlot, BookOrder>,
    current: Option<OrderSlot>,
}

impl<'a> Iterator for LevelOrdersIter<'a> {
    type Item = &'a BookOrder;

    fn next(&mut self) -> Option<Self::Item> {
        let slot = self.current?;
        let order = self.orders.get(slot)?;
        self.current = order.next();
        Some(order)
    }
}
