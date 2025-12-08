use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap, btree_map},
};

use super::*;
use alloy::primitives::Address;
use fastnum::{UD64, UD128};
use itertools::{FoldWhile, Itertools};

/// Key for L3 order time-priority ordering within a price level.
/// Tuple of (block_number, order_id) provides FIFO ordering.
pub type L3OrderKey = (u64, types::OrderId);

/// Individual order in the L3 book with denormalized account address.
#[derive(Clone, derive_more::Debug)]
pub struct L3Order {
    #[debug("{}", order.order_id())]
    order: Order,
    #[debug("{}", account_address)]
    account_address: Address,
}

impl L3Order {
    /// Create a new L3 order.
    pub fn new(order: Order, account_address: Address) -> Self {
        Self {
            order,
            account_address,
        }
    }

    /// The underlying order.
    pub fn order(&self) -> &Order {
        &self.order
    }

    /// Account address that placed this order.
    pub fn account_address(&self) -> Address {
        self.account_address
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

    /// The L3 ordering key for this order.
    pub(crate) fn key(&self) -> L3OrderKey {
        (self.order.instant().block_number(), self.order.order_id())
    }

    /// Update the underlying order (for size changes).
    pub(crate) fn update_order(&mut self, order: Order) {
        self.order = order;
    }
}

/// L3 price level containing individual orders in time-priority order.
/// Named L3Level but used internally - the book is still called L2Book for backwards compatibility.
#[derive(Clone, derive_more::Debug, Default)]
pub struct L3Level {
    /// Individual orders keyed by (block_number, order_id) for FIFO ordering.
    #[debug("{} orders", orders.len())]
    orders: BTreeMap<L3OrderKey, L3Order>,
    /// Cached aggregate: total size at this level.
    #[debug("{}", cached_size)]
    cached_size: UD64,
    /// Cached aggregate: number of orders at this level.
    cached_num_orders: u32,
}

/// BTreeMap-based L2/L3 order book.
///
/// Tracks individual orders per price level in time-priority order (L3),
/// while maintaining aggregated L2 statistics via cached values.
#[derive(Clone, derive_more::Debug, Default)]
pub struct L2Book {
    #[debug("{:?}", asks.iter().map(|(k, v)| format!("{k}: {v:?}")).collect::<Vec<_>>())]
    asks: BTreeMap<UD64, L3Level>,
    #[debug("{:?}", bids.iter().map(|(k, v)| format!("{}: {v:?}", k.0)).collect::<Vec<_>>())]
    bids: BTreeMap<Reverse<UD64>, L3Level>,
    /// Reverse index: order_id -> (price, block_number) for O(1) lookups.
    #[debug("{} orders indexed", order_index.len())]
    order_index: HashMap<types::OrderId, (UD64, u64)>,
}

impl L3Level {
    /// Create a new empty L3 level.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total size at this price level (cached, O(1)).
    pub fn size(&self) -> UD64 {
        self.cached_size
    }

    /// Number of orders at this price level (cached, O(1)).
    pub fn num_orders(&self) -> u32 {
        self.cached_num_orders
    }

    /// Iterator over individual orders in FIFO order.
    pub fn orders(&self) -> impl Iterator<Item = &L3Order> {
        self.orders.values()
    }

    /// Get a specific order by its key.
    pub fn get_order(&self, key: &L3OrderKey) -> Option<&L3Order> {
        self.orders.get(key)
    }

    /// First (oldest) order at this level.
    pub fn first_order(&self) -> Option<&L3Order> {
        self.orders.first_key_value().map(|(_, v)| v)
    }

    /// Add an order to this level.
    pub(crate) fn add_order(&mut self, l3_order: L3Order) {
        self.cached_size += l3_order.size();
        self.cached_num_orders += 1;
        self.orders.insert(l3_order.key(), l3_order);
    }

    /// Update an order's size at this level. Returns the previous size if found.
    pub(crate) fn update_order(&mut self, key: &L3OrderKey, updated_order: Order) -> Option<UD64> {
        if let Some(l3_order) = self.orders.get_mut(key) {
            let prev_size = l3_order.size();
            self.cached_size -= prev_size;
            self.cached_size += updated_order.size();
            l3_order.update_order(updated_order);
            Some(prev_size)
        } else {
            None
        }
    }

    /// Remove an order from this level. Returns the removed order if found.
    pub(crate) fn remove_order(&mut self, key: &L3OrderKey) -> Option<L3Order> {
        if let Some(l3_order) = self.orders.remove(key) {
            self.cached_size -= l3_order.size();
            self.cached_num_orders -= 1;
            Some(l3_order)
        } else {
            None
        }
    }

    /// Check if this level has no orders.
    pub(crate) fn is_empty(&self) -> bool {
        self.cached_num_orders == 0
    }
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
            .map(|(k, v)| (*k, v.cached_size))
    }

    /// Best bid price/size.
    pub fn best_bid(&self) -> Option<(UD64, UD64)> {
        self.bids
            .first_key_value()
            .map(|(k, v)| (k.0, v.cached_size))
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
    pub(crate) fn add_order(&mut self, order: &Order, account_address: Address) {
        let l3_order = L3Order::new(*order, account_address);
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
    }

    /// Update an order's size (same price level).
    pub(crate) fn update_order(&mut self, order: &Order, _prev_order: &Order) {
        // Look up the order's position via the index
        let Some(&(indexed_price, block_number)) = self.order_index.get(&order.order_id()) else {
            return; // Order not found
        };

        let key = (block_number, order.order_id());

        match order.r#type().side() {
            types::OrderSide::Ask => {
                if let Some(level) = self.asks.get_mut(&indexed_price) {
                    level.update_order(&key, *order);
                }
            }
            types::OrderSide::Bid => {
                if let Some(level) = self.bids.get_mut(&Reverse(indexed_price)) {
                    level.update_order(&key, *order);
                }
            }
        }
    }

    /// Remove an order from the book.
    pub(crate) fn remove_order(&mut self, order: &Order) {
        // Look up and remove from index
        let Some((indexed_price, block_number)) = self.order_index.remove(&order.order_id()) else {
            return; // Order not found
        };

        let key = (block_number, order.order_id());

        match order.r#type().side() {
            types::OrderSide::Ask => {
                if let btree_map::Entry::Occupied(mut o) = self.asks.entry(indexed_price) {
                    o.get_mut().remove_order(&key);
                    if o.get().is_empty() {
                        o.remove();
                    }
                }
            }
            types::OrderSide::Bid => {
                if let btree_map::Entry::Occupied(mut o) = self.bids.entry(Reverse(indexed_price)) {
                    o.get_mut().remove_order(&key);
                    if o.get().is_empty() {
                        o.remove();
                    }
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use alloy::primitives::address;
    use fastnum::udec64;

    use super::*;

    // ============================================================================
    // TEST DSL MACROS
    // ============================================================================

    /// Create an ask order (OpenShort) for L3 testing.
    /// ask!(price, size, block, order_id, account_id)
    macro_rules! ask {
        ($price:expr, $size:expr, $block:expr, $oid:expr, $aid:expr) => {
            Order::for_l3_testing(
                types::OrderType::OpenShort,
                udec64!($price),
                udec64!($size),
                $block,
                $oid,
                $aid,
            )
        };
    }

    /// Create a bid order (OpenLong) for L3 testing.
    /// bid!(price, size, block, order_id, account_id)
    macro_rules! bid {
        ($price:expr, $size:expr, $block:expr, $oid:expr, $aid:expr) => {
            Order::for_l3_testing(
                types::OrderType::OpenLong,
                udec64!($price),
                udec64!($size),
                $block,
                $oid,
                $aid,
            )
        };
    }

    /// Test addresses for different accounts.
    fn addr(n: u8) -> Address {
        Address::new([n; 20])
    }

    /// Assert L2 level state: (price, total_size, num_orders).
    macro_rules! assert_level {
        ($book:expr, ask @ $price:expr => ($size:expr, $count:expr)) => {
            let level = $book.ask_level(udec64!($price)).expect("ask level exists");
            assert_eq!(level.size(), udec64!($size), "ask@{} size", $price);
            assert_eq!(level.num_orders(), $count, "ask@{} count", $price);
        };
        ($book:expr, bid @ $price:expr => ($size:expr, $count:expr)) => {
            let level = $book.bid_level(udec64!($price)).expect("bid level exists");
            assert_eq!(level.size(), udec64!($size), "bid@{} size", $price);
            assert_eq!(level.num_orders(), $count, "bid@{} count", $price);
        };
    }

    /// Assert best ask/bid: best_ask!(book, price, size) or best_ask!(book, none).
    macro_rules! assert_best_ask {
        ($book:expr, none) => {
            assert_eq!($book.best_ask(), None, "expected no best ask");
        };
        ($book:expr, $price:expr, $size:expr) => {
            assert_eq!(
                $book.best_ask(),
                Some((udec64!($price), udec64!($size))),
                "best ask"
            );
        };
    }

    macro_rules! assert_best_bid {
        ($book:expr, none) => {
            assert_eq!($book.best_bid(), None, "expected no best bid");
        };
        ($book:expr, $price:expr, $size:expr) => {
            assert_eq!(
                $book.best_bid(),
                Some((udec64!($price), udec64!($size))),
                "best bid"
            );
        };
    }

    /// Assert order exists in book with expected properties.
    macro_rules! assert_order {
        ($book:expr, $oid:expr => { price: $price:expr, size: $size:expr, addr: $addr:expr }) => {
            let order = $book.get_order($oid).expect(&format!("order {} exists", $oid));
            assert_eq!(order.price(), udec64!($price), "order {} price", $oid);
            assert_eq!(order.size(), udec64!($size), "order {} size", $oid);
            assert_eq!(order.account_address(), $addr, "order {} address", $oid);
        };
    }

    /// Assert FIFO order at a price level.
    macro_rules! assert_fifo {
        ($book:expr, ask @ $price:expr => [$($oid:expr),*]) => {
            let level = $book.ask_level(udec64!($price)).expect("ask level exists");
            let order_ids: Vec<_> = level.orders().map(|o| o.order_id()).collect();
            assert_eq!(order_ids, vec![$($oid),*], "ask@{} FIFO order", $price);
        };
        ($book:expr, bid @ $price:expr => [$($oid:expr),*]) => {
            let level = $book.bid_level(udec64!($price)).expect("bid level exists");
            let order_ids: Vec<_> = level.orders().map(|o| o.order_id()).collect();
            assert_eq!(order_ids, vec![$($oid),*], "bid@{} FIFO order", $price);
        };
    }

    // ============================================================================
    // L3LEVEL TESTS
    // ============================================================================

    #[test]
    fn l3_level_add_single_order() {
        // Adding one order: size and count update correctly.
        let mut level = L3Level::new();
        let order = ask!(100, 1.5, 1, 1, 1);
        level.add_order(L3Order::new(order, addr(1)));

        assert_eq!(level.size(), udec64!(1.5));
        assert_eq!(level.num_orders(), 1);
        assert!(!level.is_empty());
    }

    #[test]
    fn l3_level_fifo_different_blocks() {
        // Orders from different blocks: earlier block comes first.
        let mut level = L3Level::new();
        level.add_order(L3Order::new(ask!(100, 1.0, 10, 5, 1), addr(1)));
        level.add_order(L3Order::new(ask!(100, 2.0, 5, 3, 2), addr(2))); // earlier block
        level.add_order(L3Order::new(ask!(100, 3.0, 15, 7, 3), addr(3)));

        let order_ids: Vec<_> = level.orders().map(|o| o.order_id()).collect();
        // Block 5 < 10 < 15, so order 3 first, then 5, then 7
        assert_eq!(order_ids, vec![3, 5, 7]);
        assert_eq!(level.size(), udec64!(6.0));
        assert_eq!(level.num_orders(), 3);
    }

    #[test]
    fn l3_level_fifo_same_block() {
        // Orders in same block: lower order_id comes first.
        let mut level = L3Level::new();
        level.add_order(L3Order::new(ask!(100, 1.0, 10, 5, 1), addr(1)));
        level.add_order(L3Order::new(ask!(100, 2.0, 10, 2, 2), addr(2))); // lower id
        level.add_order(L3Order::new(ask!(100, 3.0, 10, 8, 3), addr(3)));

        let order_ids: Vec<_> = level.orders().map(|o| o.order_id()).collect();
        // Same block 10, so ordered by id: 2 < 5 < 8
        assert_eq!(order_ids, vec![2, 5, 8]);
    }

    #[test]
    fn l3_level_update_order_size() {
        // Update order size: cached aggregate reflects change.
        let mut level = L3Level::new();
        let order = ask!(100, 5.0, 1, 1, 1);
        level.add_order(L3Order::new(order, addr(1)));

        let updated = order.with_size(udec64!(2.0));
        let prev_size = level.update_order(&(1, 1), updated);

        assert_eq!(prev_size, Some(udec64!(5.0)));
        assert_eq!(level.size(), udec64!(2.0));
        assert_eq!(level.num_orders(), 1); // count unchanged
    }

    #[test]
    fn l3_level_remove_order() {
        // Remove order: size and count decrease, order no longer iterable.
        let mut level = L3Level::new();
        level.add_order(L3Order::new(ask!(100, 3.0, 1, 1, 1), addr(1)));
        level.add_order(L3Order::new(ask!(100, 2.0, 2, 2, 2), addr(2)));

        let removed = level.remove_order(&(1, 1));
        assert!(removed.is_some());
        assert_eq!(level.size(), udec64!(2.0));
        assert_eq!(level.num_orders(), 1);

        let order_ids: Vec<_> = level.orders().map(|o| o.order_id()).collect();
        assert_eq!(order_ids, vec![2]);
    }

    #[test]
    fn l3_level_empty_after_removals() {
        // Level becomes empty after all orders removed.
        let mut level = L3Level::new();
        level.add_order(L3Order::new(ask!(100, 1.0, 1, 1, 1), addr(1)));

        level.remove_order(&(1, 1));
        assert!(level.is_empty());
        assert_eq!(level.size(), udec64!(0));
        assert_eq!(level.num_orders(), 0);
    }

    #[test]
    fn l3_level_first_order() {
        // first_order() returns oldest order (FIFO head).
        let mut level = L3Level::new();
        level.add_order(L3Order::new(ask!(100, 1.0, 10, 5, 1), addr(1)));
        level.add_order(L3Order::new(ask!(100, 2.0, 5, 3, 2), addr(2))); // oldest

        let first = level.first_order().expect("has orders");
        assert_eq!(first.order_id(), 3); // block 5 is earliest
    }

    // ============================================================================
    // L2BOOK TESTS - L2 API COMPATIBILITY
    // ============================================================================

    #[test]
    fn l2_book_add_ask_order() {
        // Ask orders appear in asks, not bids.
        let mut book = L2Book::new();
        book.add_order(&ask!(100, 1.0, 1, 1, 1), addr(1));

        assert_best_ask!(book, 100, 1.0);
        assert_best_bid!(book, none);
        assert_eq!(book.total_orders(), 1);
    }

    #[test]
    fn l2_book_add_bid_order() {
        // Bid orders appear in bids, not asks.
        let mut book = L2Book::new();
        book.add_order(&bid!(90, 2.0, 1, 1, 1), addr(1));

        assert_best_bid!(book, 90, 2.0);
        assert_best_ask!(book, none);
        assert_eq!(book.total_orders(), 1);
    }

    #[test]
    fn l2_book_best_prices() {
        // Best ask is lowest price, best bid is highest price.
        let mut book = L2Book::new();
        book.add_order(&ask!(110, 1.0, 1, 1, 1), addr(1));
        book.add_order(&ask!(100, 1.0, 2, 2, 2), addr(2)); // best
        book.add_order(&ask!(120, 1.0, 3, 3, 3), addr(3));
        book.add_order(&bid!(80, 1.0, 4, 4, 4), addr(4));
        book.add_order(&bid!(90, 1.0, 5, 5, 5), addr(5)); // best
        book.add_order(&bid!(70, 1.0, 6, 6, 6), addr(6));

        assert_best_ask!(book, 100, 1.0);
        assert_best_bid!(book, 90, 1.0);
    }

    #[test]
    fn l2_book_multiple_orders_same_price() {
        // Multiple orders at same price: sizes aggregate, FIFO maintained.
        let mut book = L2Book::new();
        book.add_order(&ask!(100, 1.0, 1, 1, 1), addr(1));
        book.add_order(&ask!(100, 2.0, 2, 2, 2), addr(2));
        book.add_order(&ask!(100, 3.0, 3, 3, 3), addr(3));

        assert_level!(book, ask @ 100 => (6.0, 3));
        assert_fifo!(book, ask @ 100 => [1, 2, 3]);
    }

    #[test]
    fn l2_book_ask_impact_single_level() {
        // Impact within one price level.
        let mut book = L2Book::new();
        book.add_order(&ask!(100, 5.0, 1, 1, 1), addr(1));

        let impact = book.ask_impact(udec64!(2.0));
        // (impact_price, fillable_size, vwap)
        assert_eq!(impact, Some((udec64!(100), udec64!(2.0), udec64!(100))));
    }

    #[test]
    fn l2_book_ask_impact_multiple_levels() {
        // Impact spanning multiple price levels.
        let mut book = L2Book::new();
        book.add_order(&ask!(100, 1.0, 1, 1, 1), addr(1));
        book.add_order(&ask!(110, 2.0, 2, 2, 2), addr(2));
        book.add_order(&ask!(120, 3.0, 3, 3, 3), addr(3));

        // Want 2.5: fills 1.0@100 + 1.5@110 = 100 + 165 = 265 / 2.5 = 106
        let impact = book.ask_impact(udec64!(2.5));
        assert_eq!(
            impact,
            Some((udec64!(110), udec64!(2.5), udec64!(265) / udec64!(2.5)))
        );
    }

    #[test]
    fn l2_book_bid_impact() {
        // Bid impact works similarly.
        let mut book = L2Book::new();
        book.add_order(&bid!(100, 2.0, 1, 1, 1), addr(1));
        book.add_order(&bid!(90, 3.0, 2, 2, 2), addr(2));

        // Want 3.0: fills 2.0@100 + 1.0@90 = 200 + 90 = 290 / 3.0
        let impact = book.bid_impact(udec64!(3.0));
        assert_eq!(
            impact,
            Some((udec64!(90), udec64!(3.0), udec64!(290) / udec64!(3.0)))
        );
    }

    // ============================================================================
    // L2BOOK TESTS - L3 API
    // ============================================================================

    #[test]
    fn l2_book_get_order_by_id() {
        // O(1) lookup by order_id via reverse index.
        let mut book = L2Book::new();
        book.add_order(&ask!(100, 1.0, 1, 42, 7), addr(7));

        assert_order!(book, 42 => { price: 100, size: 1.0, addr: addr(7) });
    }

    #[test]
    fn l2_book_ask_orders_iterator() {
        // Iterate all asks in price-time priority.
        let mut book = L2Book::new();
        book.add_order(&ask!(110, 1.0, 1, 1, 1), addr(1));
        book.add_order(&ask!(100, 1.0, 2, 2, 2), addr(2)); // best price
        book.add_order(&ask!(110, 1.0, 3, 3, 3), addr(3)); // same as order 1

        let order_ids: Vec<_> = book.ask_orders().map(|o| o.order_id()).collect();
        // Price 100 first, then price 110 (block 1 < block 3)
        assert_eq!(order_ids, vec![2, 1, 3]);
    }

    #[test]
    fn l2_book_bid_orders_iterator() {
        // Iterate all bids in price-time priority (highest price first).
        let mut book = L2Book::new();
        book.add_order(&bid!(90, 1.0, 1, 1, 1), addr(1));
        book.add_order(&bid!(100, 1.0, 2, 2, 2), addr(2)); // best price
        book.add_order(&bid!(90, 1.0, 3, 3, 3), addr(3));

        let order_ids: Vec<_> = book.bid_orders().map(|o| o.order_id()).collect();
        // Price 100 first, then price 90 (block 1 < block 3)
        assert_eq!(order_ids, vec![2, 1, 3]);
    }

    #[test]
    fn l2_book_level_accessors() {
        // ask_level() and bid_level() return level at specific price.
        let mut book = L2Book::new();
        book.add_order(&ask!(100, 1.0, 1, 1, 1), addr(1));
        book.add_order(&ask!(100, 2.0, 2, 2, 2), addr(2));
        book.add_order(&bid!(90, 3.0, 3, 3, 3), addr(3));

        assert!(book.ask_level(udec64!(100)).is_some());
        assert!(book.ask_level(udec64!(99)).is_none());
        assert!(book.bid_level(udec64!(90)).is_some());
        assert!(book.bid_level(udec64!(91)).is_none());
    }

    // ============================================================================
    // L2BOOK TESTS - MUTATIONS
    // ============================================================================

    #[test]
    fn l2_book_update_order_same_price() {
        // Partial fill: size decreases, count unchanged, FIFO position preserved.
        let mut book = L2Book::new();
        let order = ask!(100, 5.0, 1, 1, 1);
        book.add_order(&order, addr(1));
        book.add_order(&ask!(100, 3.0, 2, 2, 2), addr(2));

        // Order 1 partially filled: 5.0 -> 2.0
        let updated = order.with_size(udec64!(2.0));
        book.update_order(&updated, &order);

        assert_level!(book, ask @ 100 => (5.0, 2)); // 2.0 + 3.0
        assert_fifo!(book, ask @ 100 => [1, 2]); // FIFO preserved
        assert_order!(book, 1 => { price: 100, size: 2.0, addr: addr(1) });
    }

    #[test]
    fn l2_book_remove_order() {
        // Remove order: level updated, empty level pruned.
        let mut book = L2Book::new();
        let order1 = ask!(100, 2.0, 1, 1, 1);
        let order2 = ask!(100, 3.0, 2, 2, 2);
        book.add_order(&order1, addr(1));
        book.add_order(&order2, addr(2));

        book.remove_order(&order1);

        assert_level!(book, ask @ 100 => (3.0, 1));
        assert!(book.get_order(1).is_none());
        assert!(book.get_order(2).is_some());
    }

    #[test]
    fn l2_book_remove_last_order_at_level() {
        // Removing last order at a level: level is pruned from book.
        let mut book = L2Book::new();
        let order = ask!(100, 1.0, 1, 1, 1);
        book.add_order(&order, addr(1));
        book.add_order(&ask!(110, 1.0, 2, 2, 2), addr(2));

        book.remove_order(&order);

        assert!(book.ask_level(udec64!(100)).is_none());
        assert_best_ask!(book, 110, 1.0);
    }

    // ============================================================================
    // EDGE CASE TESTS
    // ============================================================================

    #[test]
    fn l2_book_empty_book_operations() {
        // Operations on empty book don't panic.
        let book = L2Book::new();

        assert_best_ask!(book, none);
        assert_best_bid!(book, none);
        assert!(book.ask_impact(udec64!(1.0)).is_none());
        assert!(book.bid_impact(udec64!(1.0)).is_none());
        assert!(book.get_order(1).is_none());
        assert_eq!(book.total_orders(), 0);
    }

    #[test]
    fn l2_book_order_id_reuse() {
        // Same order_id can be reused after removal (different block).
        let mut book = L2Book::new();
        let order1 = ask!(100, 1.0, 1, 42, 1);
        book.add_order(&order1, addr(1));
        book.remove_order(&order1);

        // New order with same ID but different block
        let order2 = ask!(110, 2.0, 5, 42, 2);
        book.add_order(&order2, addr(2));

        assert_order!(book, 42 => { price: 110, size: 2.0, addr: addr(2) });
        assert_eq!(book.total_orders(), 1);
    }

    #[test]
    fn l2_book_account_address_stored() {
        // Account address is correctly stored and retrievable.
        let mut book = L2Book::new();
        let alice = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let bob = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        book.add_order(&ask!(100, 1.0, 1, 1, 1), alice);
        book.add_order(&ask!(100, 2.0, 2, 2, 2), bob);

        let order1 = book.get_order(1).unwrap();
        let order2 = book.get_order(2).unwrap();
        assert_eq!(order1.account_address(), alice);
        assert_eq!(order2.account_address(), bob);
    }

    #[test]
    fn l2_book_remove_nonexistent() {
        // Removing nonexistent order is a no-op.
        let mut book = L2Book::new();
        let order = ask!(100, 1.0, 1, 99, 1);

        // Should not panic
        book.remove_order(&order);
        assert_eq!(book.total_orders(), 0);
    }

    #[test]
    fn l2_book_update_nonexistent() {
        // Updating nonexistent order is a no-op.
        let mut book = L2Book::new();
        let order = ask!(100, 1.0, 1, 99, 1);

        // Should not panic
        book.update_order(&order.with_size(udec64!(0.5)), &order);
        assert_eq!(book.total_orders(), 0);
    }

    // ============================================================================
    // COMPREHENSIVE SCENARIO TESTS
    // ============================================================================

    #[test]
    fn scenario_order_book_lifecycle() {
        // Full lifecycle: place -> partial fill -> cancel remaining.
        //
        // Block 1: Alice places ask 100@1.0
        // Block 2: Bob places ask 100@2.0
        // Block 3: Alice's order partially filled (1.0 -> 0.3)
        // Block 4: Alice cancels remaining
        //
        // Expected: Only Bob's order remains.

        let mut book = L2Book::new();
        let alice = addr(1);
        let bob = addr(2);

        // Block 1: Alice places
        let alice_order = ask!(100, 1.0, 1, 1, 1);
        book.add_order(&alice_order, alice);
        assert_level!(book, ask @ 100 => (1.0, 1));

        // Block 2: Bob places
        let bob_order = ask!(100, 2.0, 2, 2, 2);
        book.add_order(&bob_order, bob);
        assert_level!(book, ask @ 100 => (3.0, 2));
        assert_fifo!(book, ask @ 100 => [1, 2]);

        // Block 3: Alice partially filled
        let alice_updated = alice_order.with_size(udec64!(0.3));
        book.update_order(&alice_updated, &alice_order);
        assert_level!(book, ask @ 100 => (2.3, 2));
        assert_fifo!(book, ask @ 100 => [1, 2]); // FIFO preserved

        // Block 4: Alice cancels
        book.remove_order(&alice_updated);
        assert_level!(book, ask @ 100 => (2.0, 1));
        assert_fifo!(book, ask @ 100 => [2]);
    }

    #[test]
    fn scenario_multi_level_book() {
        // Multi-level book with asks and bids.
        //
        // Asks: 100@1.0, 110@2.0, 120@3.0
        // Bids: 90@1.5, 80@2.5, 70@3.5
        //
        // Verify all L2 and L3 accessors work correctly.

        let mut book = L2Book::new();

        // Build ask side
        book.add_order(&ask!(100, 1.0, 1, 1, 1), addr(1));
        book.add_order(&ask!(110, 2.0, 2, 2, 2), addr(2));
        book.add_order(&ask!(120, 3.0, 3, 3, 3), addr(3));

        // Build bid side
        book.add_order(&bid!(90, 1.5, 4, 4, 4), addr(4));
        book.add_order(&bid!(80, 2.5, 5, 5, 5), addr(5));
        book.add_order(&bid!(70, 3.5, 6, 6, 6), addr(6));

        // L2 checks
        assert_best_ask!(book, 100, 1.0);
        assert_best_bid!(book, 90, 1.5);
        assert_eq!(book.total_orders(), 6);

        // All 3 ask levels exist
        assert!(book.ask_level(udec64!(100)).is_some());
        assert!(book.ask_level(udec64!(110)).is_some());
        assert!(book.ask_level(udec64!(120)).is_some());

        // All 3 bid levels exist
        assert!(book.bid_level(udec64!(90)).is_some());
        assert!(book.bid_level(udec64!(80)).is_some());
        assert!(book.bid_level(udec64!(70)).is_some());

        // L3 iteration order
        let ask_ids: Vec<_> = book.ask_orders().map(|o| o.order_id()).collect();
        assert_eq!(ask_ids, vec![1, 2, 3]); // price order: 100, 110, 120

        let bid_ids: Vec<_> = book.bid_orders().map(|o| o.order_id()).collect();
        assert_eq!(bid_ids, vec![4, 5, 6]); // price order: 90, 80, 70
    }
}
