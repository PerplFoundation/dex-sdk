//! L2/L3 order book implementation.
//!
//! This module provides the order book data structure that tracks orders
//! at each price level with FIFO time-priority ordering.

mod error;
mod level;
mod order;

#[cfg(test)]
mod tests;

pub use error::{L2BookError, L2BookResult};
pub use level::L3Level;
pub use order::{L3Order, L3OrderKey};

use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap, btree_map},
};

use fastnum::{UD64, UD128};
use itertools::{FoldWhile, Itertools};

use crate::{state::Order, types};

/// BTreeMap-based L2/L3 order book.
///
/// Tracks individual orders per price level in time-priority order (L3),
/// while maintaining aggregated L2 statistics via cached values.
#[derive(Clone, Debug, Default)]
pub struct L2Book {
    asks: BTreeMap<UD64, L3Level>,
    bids: BTreeMap<Reverse<UD64>, L3Level>,
    /// Reverse index: order_id -> (price, block_number) for O(1) lookups.
    order_index: HashMap<types::OrderId, (UD64, u64)>,
}

impl L2Book {
    pub(crate) fn new() -> Self {
        Self {
            asks: BTreeMap::new(),
            bids: BTreeMap::new(),
            order_index: HashMap::new(),
        }
    }

    // === L2 API (backwards compatible) ===

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

    // === L3 API (new) ===

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
        let (price, block_number) = self.order_index.get(&order_id)?;
        let key = (*block_number, order_id);
        // Determine side by checking both - order could be on either
        if let Some(level) = self.asks.get(price) {
            if let Some(order) = level.get_order(&key) {
                return Some(order);
            }
        }
        if let Some(level) = self.bids.get(&Reverse(*price)) {
            if let Some(order) = level.get_order(&key) {
                return Some(order);
            }
        }
        None
    }

    /// Iterator over all L3 orders on the ask side in price-time priority.
    pub fn ask_orders(&self) -> impl Iterator<Item = &L3Order> {
        self.asks.values().flat_map(|level| level.orders())
    }

    /// Iterator over all L3 orders on the bid side in price-time priority.
    pub fn bid_orders(&self) -> impl Iterator<Item = &L3Order> {
        self.bids.values().flat_map(|level| level.orders())
    }

    /// Total number of orders in the book.
    pub fn total_orders(&self) -> usize {
        self.order_index.len()
    }

    // === Mutation methods ===

    /// Add an order to the book.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The order already exists in the book
    /// - The order has zero size
    /// - The order has zero price
    pub(crate) fn add_order(&mut self, order: &Order) -> L2BookResult<()> {
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
        if let Some(&(existing_price, existing_block)) = self.order_index.get(&order.order_id()) {
            return Err(L2BookError::OrderAlreadyExists {
                order_id: order.order_id(),
                existing_price,
                existing_block,
            });
        }

        let l3_order = L3Order::new(*order);
        let key = l3_order.key();

        // Update reverse index
        self.order_index
            .insert(order.order_id(), (order.price(), key.0));

        match order.r#type().side() {
            types::OrderSide::Ask => match self.asks.entry(order.price()) {
                btree_map::Entry::Vacant(v) => {
                    let mut level = L3Level::new();
                    level.add_order(l3_order);
                    v.insert(level);
                }
                btree_map::Entry::Occupied(mut o) => {
                    o.get_mut().add_order(l3_order);
                }
            },
            types::OrderSide::Bid => match self.bids.entry(Reverse(order.price())) {
                btree_map::Entry::Vacant(v) => {
                    let mut level = L3Level::new();
                    level.add_order(l3_order);
                    v.insert(level);
                }
                btree_map::Entry::Occupied(mut o) => {
                    o.get_mut().add_order(l3_order);
                }
            },
        }

        Ok(())
    }

    /// Update an order's size (same price level).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The order doesn't exist in the book
    /// - The order is not found at its expected price level (internal inconsistency)
    /// - The new size is zero
    pub(crate) fn update_order(&mut self, order: &Order, _prev_order: &Order) -> L2BookResult<()> {
        // Validate new size
        if order.size() == UD64::ZERO {
            return Err(L2BookError::InvalidOrderSize {
                order_id: order.order_id(),
                size: order.size(),
            });
        }

        // Look up the order's position via the index
        let &(indexed_price, block_number) =
            self.order_index.get(&order.order_id()).ok_or_else(|| {
                L2BookError::OrderNotFound {
                    order_id: order.order_id(),
                }
            })?;

        let key = (block_number, order.order_id());
        let side = order.r#type().side();

        match side {
            types::OrderSide::Ask => {
                let level = self.asks.get_mut(&indexed_price).ok_or_else(|| {
                    L2BookError::OrderNotAtExpectedLevel {
                        order_id: order.order_id(),
                        expected_price: indexed_price,
                        side,
                    }
                })?;
                level.update_order(&key, *order).ok_or_else(|| {
                    L2BookError::OrderNotAtExpectedLevel {
                        order_id: order.order_id(),
                        expected_price: indexed_price,
                        side,
                    }
                })?;
            }
            types::OrderSide::Bid => {
                let level =
                    self.bids
                        .get_mut(&Reverse(indexed_price))
                        .ok_or_else(|| L2BookError::OrderNotAtExpectedLevel {
                            order_id: order.order_id(),
                            expected_price: indexed_price,
                            side,
                        })?;
                level.update_order(&key, *order).ok_or_else(|| {
                    L2BookError::OrderNotAtExpectedLevel {
                        order_id: order.order_id(),
                        expected_price: indexed_price,
                        side,
                    }
                })?;
            }
        }

        Ok(())
    }

    /// Remove an order from the book.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The order doesn't exist in the book
    /// - The order is not found at its expected price level (internal inconsistency)
    pub(crate) fn remove_order(&mut self, order: &Order) -> L2BookResult<()> {
        // Look up and remove from index
        let (indexed_price, block_number) =
            self.order_index
                .remove(&order.order_id())
                .ok_or_else(|| L2BookError::OrderNotFound {
                    order_id: order.order_id(),
                })?;

        let key = (block_number, order.order_id());
        let side = order.r#type().side();

        match side {
            types::OrderSide::Ask => {
                let btree_map::Entry::Occupied(mut o) = self.asks.entry(indexed_price) else {
                    // Re-insert into index since we failed to remove
                    self.order_index
                        .insert(order.order_id(), (indexed_price, block_number));
                    return Err(L2BookError::OrderNotAtExpectedLevel {
                        order_id: order.order_id(),
                        expected_price: indexed_price,
                        side,
                    });
                };
                o.get_mut().remove_order(&key);
                if o.get().is_empty() {
                    o.remove();
                }
            }
            types::OrderSide::Bid => {
                let btree_map::Entry::Occupied(mut o) = self.bids.entry(Reverse(indexed_price))
                else {
                    // Re-insert into index since we failed to remove
                    self.order_index
                        .insert(order.order_id(), (indexed_price, block_number));
                    return Err(L2BookError::OrderNotAtExpectedLevel {
                        order_id: order.order_id(),
                        expected_price: indexed_price,
                        side,
                    });
                };
                o.get_mut().remove_order(&key);
                if o.get().is_empty() {
                    o.remove();
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
