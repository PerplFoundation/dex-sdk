//! Order book implementation with intrusive linked lists.
//!
//! This module provides the order book data structure that tracks orders
//! at each price level with FIFO time-priority ordering using doubly-linked lists.

mod error;
mod level;
mod order;

#[cfg(test)]
mod tests;

pub use error::{OrderBookError, OrderBookResult};
pub use level::BookLevel;
pub use order::BookOrder;

use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
};

use fastnum::{UD64, UD128};
use itertools::{FoldWhile, Itertools};

use crate::{state::Order, types};

/// L2/L3 order book with intrusive linked lists.
///
/// Orders are stored in a HashMap keyed by OrderId, with each price level
/// maintaining a doubly-linked list of orders in FIFO (time-priority) order.
#[derive(Clone, Debug, Default)]
pub struct OrderBook {
    /// Storage for all orders, keyed by OrderId.
    orders: HashMap<types::OrderId, BookOrder>,
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

    /// Get a specific order by ID (O(1) via HashMap lookup).
    pub fn get_order(&self, order_id: types::OrderId) -> Option<&BookOrder> {
        self.orders.get(&order_id)
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
    pub(crate) fn level_orders<'a>(&'a self, level: &'a BookLevel) -> LevelOrdersIter<'a> {
        LevelOrdersIter {
            orders: &self.orders,
            current: level.head(),
        }
    }

    /// Total number of orders in the book.
    pub fn total_orders(&self) -> usize {
        self.orders.len()
    }

    /// Access to all orders in the book.
    pub(crate) fn all_orders(&self) -> &HashMap<types::OrderId, BookOrder> {
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
    pub(crate) fn add_order(&mut self, order: &Order) -> OrderBookResult<()> {
        let order_id = order.order_id();

        // Validate order
        if order.size() == UD64::ZERO {
            return Err(OrderBookError::InvalidOrderSize {
                order_id,
                size: order.size(),
            });
        }
        if order.price() == UD64::ZERO {
            return Err(OrderBookError::InvalidOrderPrice {
                order_id,
                price: order.price(),
            });
        }

        // Check if order already exists
        if let Some(existing) = self.orders.get(&order_id) {
            return Err(OrderBookError::OrderAlreadyExists {
                order_id,
                existing_price: existing.price(),
            });
        }

        // Get or create the level and capture tail before inserting
        let side = order.r#type().side();
        let old_tail = self.get_or_create_level_mut(side, order.price()).tail();

        // Create the BookOrder with prev pointing to current tail
        let mut l3_order = BookOrder::new(*order);
        l3_order.set_prev(old_tail);

        // Insert into hashmap
        self.orders.insert(order_id, l3_order);

        // Link at tail
        self.link_at_tail(side, order.price(), old_tail, order_id, order.size());

        Ok(())
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
        let order_id = order.order_id();

        // Validate new size
        if order.size() == UD64::ZERO {
            return Err(OrderBookError::InvalidOrderSize {
                order_id,
                size: order.size(),
            });
        }

        // Find the order
        let l3_order = self
            .orders
            .get_mut(&order_id)
            .ok_or(OrderBookError::OrderNotFound { order_id })?;

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
        // Get order info before removal
        let l3_order = self
            .orders
            .get(&order_id)
            .ok_or(OrderBookError::OrderNotFound { order_id })?;

        let prev_id = l3_order.prev();
        let next_id = l3_order.next();
        let price = l3_order.price();
        let size = l3_order.size();
        let side = l3_order.r#type().side();

        // Unlink from list
        self.unlink_node(prev_id, next_id);

        // Update level head/tail and check if empty
        let level = self
            .get_level_mut(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?;
        if level.head() == Some(order_id) {
            level.set_head(next_id);
        }
        if level.tail() == Some(order_id) {
            level.set_tail(prev_id);
        }
        level.sub_size(size);
        let should_remove_level = level.is_empty();

        // Prune empty level
        if should_remove_level {
            self.remove_level(side, price);
        }

        // Remove from hashmap and return the order
        let removed = self
            .orders
            .remove(&order_id)
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
        let order_id = order.order_id();

        // Find the order
        let l3_order = self
            .orders
            .get(&order_id)
            .ok_or(OrderBookError::OrderNotFound { order_id })?;

        let prev_id = l3_order.prev();
        let next_id = l3_order.next();
        let price = l3_order.price();
        let old_size = l3_order.size();
        let side = l3_order.r#type().side();

        // If already at tail, just update the order data
        let is_at_tail = self
            .get_level(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?
            .tail()
            == Some(order_id);

        if is_at_tail {
            // Already at back, just update order data
            if let Some(l3_order) = self.orders.get_mut(&order_id) {
                l3_order.update_order(*order);
            }
            let level = self
                .get_level_mut(side, price)
                .ok_or(OrderBookError::LevelNotFound { price, side })?;
            level.update_size(old_size, order.size());
            return Ok(());
        }

        // Unlink from current position
        self.unlink_node(prev_id, next_id);

        // Update level head if we were the head
        let level = self
            .get_level_mut(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?;
        if level.head() == Some(order_id) {
            level.set_head(next_id);
        }

        // Get old tail before updating
        let old_tail = level.tail();

        // Update old tail's next pointer
        if let Some(old_tail_id) = old_tail {
            if let Some(old_tail_order) = self.orders.get_mut(&old_tail_id) {
                old_tail_order.set_next(Some(order_id));
            }
        }

        // Update this order's links and data
        if let Some(l3_order) = self.orders.get_mut(&order_id) {
            l3_order.set_prev(old_tail);
            l3_order.set_next(None);
            l3_order.update_order(*order);
        }

        // Update level tail and size - need to re-borrow
        let level = self
            .get_level_mut(side, price)
            .ok_or(OrderBookError::LevelNotFound { price, side })?;
        level.set_tail(Some(order_id));
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
        // Collect order IDs for validation
        let order_ids: std::collections::HashSet<types::OrderId> =
            orders.iter().map(|o| o.order_id()).collect();

        // First pass: insert all orders and set prev/next pointers directly
        for order in orders {
            let order_id = order.order_id();

            if order.size() == UD64::ZERO {
                return Err(OrderBookError::InvalidOrderSize {
                    order_id,
                    size: order.size(),
                });
            }
            if order.price() == UD64::ZERO {
                return Err(OrderBookError::InvalidOrderPrice {
                    order_id,
                    price: order.price(),
                });
            }

            // Validate that referenced orders exist in this snapshot
            if let Some(prev_id) = order.prev_order_id() {
                if !order_ids.contains(&prev_id) {
                    return Err(OrderBookError::DanglingOrderReference {
                        order_id,
                        referenced_id: prev_id,
                        pointer: "prev",
                    });
                }
            }
            if let Some(next_id) = order.next_order_id() {
                if !order_ids.contains(&next_id) {
                    return Err(OrderBookError::DanglingOrderReference {
                        order_id,
                        referenced_id: next_id,
                        pointer: "next",
                    });
                }
            }

            // Create BookOrder with prev/next pointing directly to OrderIds
            let mut l3_order = BookOrder::new(*order);
            l3_order.set_prev(order.prev_order_id());
            l3_order.set_next(order.next_order_id());

            self.orders.insert(order_id, l3_order);
        }

        // Second pass: build levels with head/tail and cached aggregates
        // Group orders by (price, side)
        let mut level_orders: HashMap<(UD64, types::OrderSide), Vec<types::OrderId>> =
            HashMap::new();
        for order in orders {
            let key = (order.price(), order.r#type().side());
            level_orders.entry(key).or_default().push(order.order_id());
        }

        for ((price, side), order_ids) in level_orders {
            // Find head (order with no prev in this level)
            let head = order_ids.iter().find(|&&id| {
                self.orders
                    .get(&id)
                    .is_some_and(|o| o.prev().is_none_or(|p| !order_ids.contains(&p)))
            });

            // Find tail (order with no next in this level)
            let tail = order_ids.iter().find(|&&id| {
                self.orders
                    .get(&id)
                    .is_some_and(|o| o.next().is_none_or(|n| !order_ids.contains(&n)))
            });

            // Build level with head/tail and cached aggregates
            let mut level = BookLevel::new();
            level.set_head(head.copied());
            level.set_tail(tail.copied());
            for &id in &order_ids {
                if let Some(order) = self.orders.get(&id) {
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
    fn unlink_node(&mut self, prev_id: Option<types::OrderId>, next_id: Option<types::OrderId>) {
        // Update prev's next pointer
        if let Some(prev) = prev_id {
            if let Some(prev_order) = self.orders.get_mut(&prev) {
                prev_order.set_next(next_id);
            }
        }

        // Update next's prev pointer
        if let Some(next) = next_id {
            if let Some(next_order) = self.orders.get_mut(&next) {
                next_order.set_prev(prev_id);
            }
        }
    }

    /// Link a new order at the tail of a level.
    /// Takes old_tail to avoid borrowing level while mutating orders.
    fn link_at_tail(
        &mut self,
        side: types::OrderSide,
        price: UD64,
        old_tail: Option<types::OrderId>,
        order_id: types::OrderId,
        size: UD64,
    ) {
        // Update old tail's next pointer
        if let Some(old_tail_id) = old_tail {
            if let Some(old_tail_order) = self.orders.get_mut(&old_tail_id) {
                old_tail_order.set_next(Some(order_id));
            }
        }

        // Update level head/tail (re-borrow level after updating orders)
        let level = self.get_or_create_level_mut(side, price);
        if level.head().is_none() {
            level.set_head(Some(order_id));
        }
        level.set_tail(Some(order_id));
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
    orders: &'a HashMap<types::OrderId, BookOrder>,
    current: Option<types::OrderId>,
}

impl<'a> Iterator for LevelOrdersIter<'a> {
    type Item = &'a BookOrder;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.current?;
        let order = self.orders.get(&id)?;
        self.current = order.next();
        Some(order)
    }
}
