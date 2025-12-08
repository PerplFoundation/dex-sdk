//! Tests for the L2/L3 order book.

use fastnum::udec64;

use super::*;
use crate::state::Order;

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
    ($book:expr, $oid:expr => { price: $price:expr, size: $size:expr, account_id: $aid:expr }) => {
        let order = $book.get_order($oid).expect(&format!("order {} exists", $oid));
        assert_eq!(order.price(), udec64!($price), "order {} price", $oid);
        assert_eq!(order.size(), udec64!($size), "order {} size", $oid);
        assert_eq!(order.account_id(), $aid, "order {} account_id", $oid);
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
    level.add_order(L3Order::new(order));

    assert_eq!(level.size(), udec64!(1.5));
    assert_eq!(level.num_orders(), 1);
    assert!(!level.is_empty());
}

#[test]
fn l3_level_fifo_different_blocks() {
    // Orders from different blocks: earlier block comes first.
    let mut level = L3Level::new();
    level.add_order(L3Order::new(ask!(100, 1.0, 10, 5, 1)));
    level.add_order(L3Order::new(ask!(100, 2.0, 5, 3, 2))); // earlier block
    level.add_order(L3Order::new(ask!(100, 3.0, 15, 7, 3)));

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
    level.add_order(L3Order::new(ask!(100, 1.0, 10, 5, 1)));
    level.add_order(L3Order::new(ask!(100, 2.0, 10, 2, 2))); // lower id
    level.add_order(L3Order::new(ask!(100, 3.0, 10, 8, 3)));

    let order_ids: Vec<_> = level.orders().map(|o| o.order_id()).collect();
    // Same block 10, so ordered by id: 2 < 5 < 8
    assert_eq!(order_ids, vec![2, 5, 8]);
}

#[test]
fn l3_level_update_order_size() {
    // Update order size: cached aggregate reflects change.
    let mut level = L3Level::new();
    let order = ask!(100, 5.0, 1, 1, 1);
    level.add_order(L3Order::new(order));

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
    level.add_order(L3Order::new(ask!(100, 3.0, 1, 1, 1)));
    level.add_order(L3Order::new(ask!(100, 2.0, 2, 2, 2)));

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
    level.add_order(L3Order::new(ask!(100, 1.0, 1, 1, 1)));

    level.remove_order(&(1, 1));
    assert!(level.is_empty());
    assert_eq!(level.size(), udec64!(0));
    assert_eq!(level.num_orders(), 0);
}

#[test]
fn l3_level_first_order() {
    // first_order() returns oldest order (FIFO head).
    let mut level = L3Level::new();
    level.add_order(L3Order::new(ask!(100, 1.0, 10, 5, 1)));
    level.add_order(L3Order::new(ask!(100, 2.0, 5, 3, 2))); // oldest

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
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();

    assert_best_ask!(book, 100, 1.0);
    assert_best_bid!(book, none);
    assert_eq!(book.total_orders(), 1);
}

#[test]
fn l2_book_add_bid_order() {
    // Bid orders appear in bids, not asks.
    let mut book = L2Book::new();
    book.add_order(&bid!(90, 2.0, 1, 1, 1)).unwrap();

    assert_best_bid!(book, 90, 2.0);
    assert_best_ask!(book, none);
    assert_eq!(book.total_orders(), 1);
}

#[test]
fn l2_book_best_prices() {
    // Best ask is lowest price, best bid is highest price.
    let mut book = L2Book::new();
    book.add_order(&ask!(110, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 1.0, 2, 2, 2)).unwrap(); // best
    book.add_order(&ask!(120, 1.0, 3, 3, 3)).unwrap();
    book.add_order(&bid!(80, 1.0, 4, 4, 4)).unwrap();
    book.add_order(&bid!(90, 1.0, 5, 5, 5)).unwrap(); // best
    book.add_order(&bid!(70, 1.0, 6, 6, 6)).unwrap();

    assert_best_ask!(book, 100, 1.0);
    assert_best_bid!(book, 90, 1.0);
}

#[test]
fn l2_book_multiple_orders_same_price() {
    // Multiple orders at same price: sizes aggregate, FIFO maintained.
    let mut book = L2Book::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 2.0, 2, 2, 2)).unwrap();
    book.add_order(&ask!(100, 3.0, 3, 3, 3)).unwrap();

    assert_level!(book, ask @ 100 => (6.0, 3));
    assert_fifo!(book, ask @ 100 => [1, 2, 3]);
}

#[test]
fn l2_book_ask_impact_single_level() {
    // Impact within one price level.
    let mut book = L2Book::new();
    book.add_order(&ask!(100, 5.0, 1, 1, 1)).unwrap();

    let impact = book.ask_impact(udec64!(2.0));
    // (impact_price, fillable_size, vwap)
    assert_eq!(impact, Some((udec64!(100), udec64!(2.0), udec64!(100))));
}

#[test]
fn l2_book_ask_impact_multiple_levels() {
    // Impact spanning multiple price levels.
    let mut book = L2Book::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(110, 2.0, 2, 2, 2)).unwrap();
    book.add_order(&ask!(120, 3.0, 3, 3, 3)).unwrap();

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
    book.add_order(&bid!(100, 2.0, 1, 1, 1)).unwrap();
    book.add_order(&bid!(90, 3.0, 2, 2, 2)).unwrap();

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
    book.add_order(&ask!(100, 1.0, 1, 42, 7)).unwrap();

    assert_order!(book, 42 => { price: 100, size: 1.0, account_id: 7 });
}

#[test]
fn l2_book_ask_orders_iterator() {
    // Iterate all asks in price-time priority.
    let mut book = L2Book::new();
    book.add_order(&ask!(110, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 1.0, 2, 2, 2)).unwrap(); // best price
    book.add_order(&ask!(110, 1.0, 3, 3, 3)).unwrap(); // same as order 1

    let order_ids: Vec<_> = book.ask_orders().map(|o| o.order_id()).collect();
    // Price 100 first, then price 110 (block 1 < block 3)
    assert_eq!(order_ids, vec![2, 1, 3]);
}

#[test]
fn l2_book_bid_orders_iterator() {
    // Iterate all bids in price-time priority (highest price first).
    let mut book = L2Book::new();
    book.add_order(&bid!(90, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&bid!(100, 1.0, 2, 2, 2)).unwrap(); // best price
    book.add_order(&bid!(90, 1.0, 3, 3, 3)).unwrap();

    let order_ids: Vec<_> = book.bid_orders().map(|o| o.order_id()).collect();
    // Price 100 first, then price 90 (block 1 < block 3)
    assert_eq!(order_ids, vec![2, 1, 3]);
}

#[test]
fn l2_book_level_accessors() {
    // ask_level() and bid_level() return level at specific price.
    let mut book = L2Book::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 2.0, 2, 2, 2)).unwrap();
    book.add_order(&bid!(90, 3.0, 3, 3, 3)).unwrap();

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
    book.add_order(&order).unwrap();
    book.add_order(&ask!(100, 3.0, 2, 2, 2)).unwrap();

    // Order 1 partially filled: 5.0 -> 2.0
    let updated = order.with_size(udec64!(2.0));
    book.update_order(&updated, &order).unwrap();

    assert_level!(book, ask @ 100 => (5.0, 2)); // 2.0 + 3.0
    assert_fifo!(book, ask @ 100 => [1, 2]); // FIFO preserved
    assert_order!(book, 1 => { price: 100, size: 2.0, account_id: 1 });
}

#[test]
fn l2_book_remove_order() {
    // Remove order: level updated, empty level pruned.
    let mut book = L2Book::new();
    let order1 = ask!(100, 2.0, 1, 1, 1);
    let order2 = ask!(100, 3.0, 2, 2, 2);
    book.add_order(&order1).unwrap();
    book.add_order(&order2).unwrap();

    book.remove_order(&order1).unwrap();

    assert_level!(book, ask @ 100 => (3.0, 1));
    assert!(book.get_order(1).is_none());
    assert!(book.get_order(2).is_some());
}

#[test]
fn l2_book_remove_last_order_at_level() {
    // Removing last order at a level: level is pruned from book.
    let mut book = L2Book::new();
    let order = ask!(100, 1.0, 1, 1, 1);
    book.add_order(&order).unwrap();
    book.add_order(&ask!(110, 1.0, 2, 2, 2)).unwrap();

    book.remove_order(&order).unwrap();

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
    book.add_order(&order1).unwrap();
    book.remove_order(&order1).unwrap();

    // New order with same ID but different block
    let order2 = ask!(110, 2.0, 5, 42, 2);
    book.add_order(&order2).unwrap();

    assert_order!(book, 42 => { price: 110, size: 2.0, account_id: 2 });
    assert_eq!(book.total_orders(), 1);
}

#[test]
fn l2_book_account_id_stored() {
    // Account ID is correctly stored and retrievable.
    let mut book = L2Book::new();

    book.add_order(&ask!(100, 1.0, 1, 1, 10)).unwrap(); // account_id = 10
    book.add_order(&ask!(100, 2.0, 2, 2, 20)).unwrap(); // account_id = 20

    let order1 = book.get_order(1).unwrap();
    let order2 = book.get_order(2).unwrap();
    assert_eq!(order1.account_id(), 10);
    assert_eq!(order2.account_id(), 20);
}

#[test]
fn l2_book_remove_nonexistent() {
    // Removing nonexistent order returns an error.
    let mut book = L2Book::new();
    let order = ask!(100, 1.0, 1, 99, 1);

    let result = book.remove_order(&order);
    assert!(matches!(result, Err(L2BookError::OrderNotFound { order_id: 99 })));
    assert_eq!(book.total_orders(), 0);
}

#[test]
fn l2_book_update_nonexistent() {
    // Updating nonexistent order returns an error.
    let mut book = L2Book::new();
    let order = ask!(100, 1.0, 1, 99, 1);

    let result = book.update_order(&order.with_size(udec64!(0.5)), &order);
    assert!(matches!(result, Err(L2BookError::OrderNotFound { order_id: 99 })));
    assert_eq!(book.total_orders(), 0);
}

// ============================================================================
// ERROR CASE TESTS
// ============================================================================

#[test]
fn l2_book_error_add_duplicate_order() {
    // Adding an order with the same ID twice returns an error.
    let mut book = L2Book::new();
    let order = ask!(100, 1.0, 1, 42, 1);

    book.add_order(&order).unwrap();
    let result = book.add_order(&order);

    assert!(matches!(
        result,
        Err(L2BookError::OrderAlreadyExists {
            order_id: 42,
            existing_price,
            existing_block: 1
        }) if existing_price == udec64!(100)
    ));
    assert_eq!(book.total_orders(), 1);
}

#[test]
fn l2_book_error_add_zero_size() {
    // Adding an order with zero size returns an error.
    let mut book = L2Book::new();
    let order = ask!(100, 0.0, 1, 1, 1);

    let result = book.add_order(&order);
    assert!(matches!(
        result,
        Err(L2BookError::InvalidOrderSize { order_id: 1, size }) if size == udec64!(0)
    ));
    assert_eq!(book.total_orders(), 0);
}

#[test]
fn l2_book_error_add_zero_price() {
    // Adding an order with zero price returns an error.
    let mut book = L2Book::new();
    let order = ask!(0, 1.0, 1, 1, 1);

    let result = book.add_order(&order);
    assert!(matches!(
        result,
        Err(L2BookError::InvalidOrderPrice { order_id: 1, price }) if price == udec64!(0)
    ));
    assert_eq!(book.total_orders(), 0);
}

#[test]
fn l2_book_error_update_to_zero_size() {
    // Updating an order to zero size returns an error.
    let mut book = L2Book::new();
    let order = ask!(100, 1.0, 1, 1, 1);
    book.add_order(&order).unwrap();

    let updated = order.with_size(udec64!(0.0));
    let result = book.update_order(&updated, &order);

    assert!(matches!(
        result,
        Err(L2BookError::InvalidOrderSize { order_id: 1, size }) if size == udec64!(0)
    ));
    // Order should still exist with original size
    assert_order!(book, 1 => { price: 100, size: 1.0, account_id: 1 });
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

    // Block 1: Alice places
    let alice_order = ask!(100, 1.0, 1, 1, 1);
    book.add_order(&alice_order).unwrap();
    assert_level!(book, ask @ 100 => (1.0, 1));

    // Block 2: Bob places
    let bob_order = ask!(100, 2.0, 2, 2, 2);
    book.add_order(&bob_order).unwrap();
    assert_level!(book, ask @ 100 => (3.0, 2));
    assert_fifo!(book, ask @ 100 => [1, 2]);

    // Block 3: Alice partially filled
    let alice_updated = alice_order.with_size(udec64!(0.3));
    book.update_order(&alice_updated, &alice_order).unwrap();
    assert_level!(book, ask @ 100 => (2.3, 2));
    assert_fifo!(book, ask @ 100 => [1, 2]); // FIFO preserved

    // Block 4: Alice cancels
    book.remove_order(&alice_updated).unwrap();
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
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(110, 2.0, 2, 2, 2)).unwrap();
    book.add_order(&ask!(120, 3.0, 3, 3, 3)).unwrap();

    // Build bid side
    book.add_order(&bid!(90, 1.5, 4, 4, 4)).unwrap();
    book.add_order(&bid!(80, 2.5, 5, 5, 5)).unwrap();
    book.add_order(&bid!(70, 3.5, 6, 6, 6)).unwrap();

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
