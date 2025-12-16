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
        let order_ids: Vec<_> = $book.level_orders(level).map(|o| o.order_id()).collect();
        assert_eq!(order_ids, vec![$($oid),*], "ask@{} FIFO order", $price);
    };
    ($book:expr, bid @ $price:expr => [$($oid:expr),*]) => {
        let level = $book.bid_level(udec64!($price)).expect("bid level exists");
        let order_ids: Vec<_> = $book.level_orders(level).map(|o| o.order_id()).collect();
        assert_eq!(order_ids, vec![$($oid),*], "bid@{} FIFO order", $price);
    };
}

// ============================================================================
// L3LEVEL TESTS
// ============================================================================

#[test]
fn l3_level_new_is_empty() {
    let level = BookLevel::new();
    assert!(level.is_empty());
    assert_eq!(level.size(), udec64!(0));
    assert_eq!(level.num_orders(), 0);
    assert!(level.head().is_none());
    assert!(level.tail().is_none());
}

// ============================================================================
// L2BOOK TESTS - L2 API COMPATIBILITY
// ============================================================================

#[test]
fn l2_book_add_ask_order() {
    // Ask orders appear in asks, not bids.
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();

    assert_best_ask!(book, 100, 1.0);
    assert_best_bid!(book, none);
    assert_eq!(book.total_orders(), 1);
}

#[test]
fn l2_book_add_bid_order() {
    // Bid orders appear in bids, not asks.
    let mut book = OrderBook::new();
    book.add_order(&bid!(90, 2.0, 1, 1, 1)).unwrap();

    assert_best_bid!(book, 90, 2.0);
    assert_best_ask!(book, none);
    assert_eq!(book.total_orders(), 1);
}

#[test]
fn l2_book_best_prices() {
    // Best ask is lowest price, best bid is highest price.
    let mut book = OrderBook::new();
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
    // Multiple orders at same price: sizes aggregate, FIFO by insertion order.
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 2.0, 2, 2, 2)).unwrap();
    book.add_order(&ask!(100, 3.0, 3, 3, 3)).unwrap();

    assert_level!(book, ask @ 100 => (6.0, 3));
    assert_fifo!(book, ask @ 100 => [1, 2, 3]); // insertion order
}

#[test]
fn l2_book_ask_impact_single_level() {
    // Impact within one price level.
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 5.0, 1, 1, 1)).unwrap();

    let impact = book.ask_impact(udec64!(2.0));
    // (impact_price, fillable_size, vwap)
    assert_eq!(impact, Some((udec64!(100), udec64!(2.0), udec64!(100))));
}

#[test]
fn l2_book_ask_impact_multiple_levels() {
    // Impact spanning multiple price levels.
    let mut book = OrderBook::new();
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
    let mut book = OrderBook::new();
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
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 1.0, 1, 42, 7)).unwrap();

    assert_order!(book, 42 => { price: 100, size: 1.0, account_id: 7 });
}

#[test]
fn l2_book_ask_orders_iterator() {
    // Iterate all asks in price-time priority.
    let mut book = OrderBook::new();
    book.add_order(&ask!(110, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 1.0, 2, 2, 2)).unwrap(); // best price
    book.add_order(&ask!(110, 1.0, 3, 3, 3)).unwrap(); // same as order 1

    let order_ids: Vec<_> = book.ask_orders().map(|o| o.order_id()).collect();
    // Price 100 first, then price 110 (insertion order within price level: 1, 3)
    assert_eq!(order_ids, vec![2, 1, 3]);
}

#[test]
fn l2_book_bid_orders_iterator() {
    // Iterate all bids in price-time priority (highest price first).
    let mut book = OrderBook::new();
    book.add_order(&bid!(90, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&bid!(100, 1.0, 2, 2, 2)).unwrap(); // best price
    book.add_order(&bid!(90, 1.0, 3, 3, 3)).unwrap();

    let order_ids: Vec<_> = book.bid_orders().map(|o| o.order_id()).collect();
    // Price 100 first, then price 90 (insertion order: 1, 3)
    assert_eq!(order_ids, vec![2, 1, 3]);
}

#[test]
fn l2_book_level_accessors() {
    // ask_level() and bid_level() return level at specific price.
    let mut book = OrderBook::new();
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
    let mut book = OrderBook::new();
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
    let mut book = OrderBook::new();
    let order1 = ask!(100, 2.0, 1, 1, 1);
    let order2 = ask!(100, 3.0, 2, 2, 2);
    book.add_order(&order1).unwrap();
    book.add_order(&order2).unwrap();

    book.remove_order_by_id(order1.order_id()).unwrap();

    assert_level!(book, ask @ 100 => (3.0, 1));
    assert!(book.get_order(1).is_none());
    assert!(book.get_order(2).is_some());
}

#[test]
fn l2_book_remove_last_order_at_level() {
    // Removing last order at a level: level is pruned from book.
    let mut book = OrderBook::new();
    let order = ask!(100, 1.0, 1, 1, 1);
    book.add_order(&order).unwrap();
    book.add_order(&ask!(110, 1.0, 2, 2, 2)).unwrap();

    book.remove_order_by_id(order.order_id()).unwrap();

    assert!(book.ask_level(udec64!(100)).is_none());
    assert_best_ask!(book, 110, 1.0);
}

// ============================================================================
// L2BOOK TESTS - MOVE TO BACK (SIZE INCREASE)
// ============================================================================

#[test]
fn l2_book_move_to_back() {
    // Size increase should move order to back of queue.
    let mut book = OrderBook::new();
    let order1 = ask!(100, 1.0, 1, 1, 1);
    let order2 = ask!(100, 2.0, 2, 2, 2);
    let order3 = ask!(100, 3.0, 3, 3, 3);
    book.add_order(&order1).unwrap();
    book.add_order(&order2).unwrap();
    book.add_order(&order3).unwrap();

    // Initial FIFO: [1, 2, 3]
    assert_fifo!(book, ask @ 100 => [1, 2, 3]);

    // Move order 1 to back (simulating size increase)
    let order1_updated = order1.with_size(udec64!(1.5));
    book.move_to_back(&order1_updated, &order1).unwrap();

    // New FIFO: [2, 3, 1]
    assert_fifo!(book, ask @ 100 => [2, 3, 1]);
    assert_level!(book, ask @ 100 => (6.5, 3)); // 2.0 + 3.0 + 1.5
    assert_order!(book, 1 => { price: 100, size: 1.5, account_id: 1 });
}

#[test]
fn l2_book_move_to_back_middle_order() {
    // Move middle order to back.
    let mut book = OrderBook::new();
    let order1 = ask!(100, 1.0, 1, 1, 1);
    let order2 = ask!(100, 2.0, 2, 2, 2);
    let order3 = ask!(100, 3.0, 3, 3, 3);
    book.add_order(&order1).unwrap();
    book.add_order(&order2).unwrap();
    book.add_order(&order3).unwrap();

    // Move order 2 to back
    let order2_updated = order2.with_size(udec64!(2.5));
    book.move_to_back(&order2_updated, &order2).unwrap();

    // New FIFO: [1, 3, 2]
    assert_fifo!(book, ask @ 100 => [1, 3, 2]);
}

// ============================================================================
// EDGE CASE TESTS
// ============================================================================

#[test]
fn l2_book_empty_book_operations() {
    // Operations on empty book don't panic.
    let book = OrderBook::new();

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
    let mut book = OrderBook::new();
    let order1 = ask!(100, 1.0, 1, 42, 1);
    book.add_order(&order1).unwrap();
    book.remove_order_by_id(order1.order_id()).unwrap();

    // New order with same ID but different block
    let order2 = ask!(110, 2.0, 5, 42, 2);
    book.add_order(&order2).unwrap();

    assert_order!(book, 42 => { price: 110, size: 2.0, account_id: 2 });
    assert_eq!(book.total_orders(), 1);
}

#[test]
fn l2_book_account_id_stored() {
    // Account ID is correctly stored and retrievable.
    let mut book = OrderBook::new();

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
    let mut book = OrderBook::new();

    let result = book.remove_order_by_id(99);
    assert!(matches!(result, Err(OrderBookError::OrderNotFound { order_id: 99 })));
    assert_eq!(book.total_orders(), 0);
}

#[test]
fn l2_book_update_nonexistent() {
    // Updating nonexistent order returns an error.
    let mut book = OrderBook::new();
    let order = ask!(100, 1.0, 1, 99, 1);

    let result = book.update_order(&order.with_size(udec64!(0.5)), &order);
    assert!(matches!(result, Err(OrderBookError::OrderNotFound { order_id: 99 })));
    assert_eq!(book.total_orders(), 0);
}

// ============================================================================
// ERROR CASE TESTS
// ============================================================================

#[test]
fn l2_book_error_add_duplicate_order() {
    // Adding an order with the same ID twice returns an error.
    let mut book = OrderBook::new();
    let order = ask!(100, 1.0, 1, 42, 1);

    book.add_order(&order).unwrap();
    let result = book.add_order(&order);

    assert!(matches!(
        result,
        Err(OrderBookError::OrderAlreadyExists {
            order_id: 42,
            existing_price,
            ..
        }) if existing_price == udec64!(100)
    ));
    assert_eq!(book.total_orders(), 1);
}

#[test]
fn l2_book_error_add_zero_size() {
    // Adding an order with zero size returns an error.
    let mut book = OrderBook::new();
    let order = ask!(100, 0.0, 1, 1, 1);

    let result = book.add_order(&order);
    assert!(matches!(
        result,
        Err(OrderBookError::InvalidOrderSize { order_id: 1, size }) if size == udec64!(0)
    ));
    assert_eq!(book.total_orders(), 0);
}

#[test]
fn l2_book_error_add_zero_price() {
    // Adding an order with zero price returns an error.
    let mut book = OrderBook::new();
    let order = ask!(0, 1.0, 1, 1, 1);

    let result = book.add_order(&order);
    assert!(matches!(
        result,
        Err(OrderBookError::InvalidOrderPrice { order_id: 1, price }) if price == udec64!(0)
    ));
    assert_eq!(book.total_orders(), 0);
}

#[test]
fn l2_book_error_update_to_zero_size() {
    // Updating an order to zero size returns an error.
    let mut book = OrderBook::new();
    let order = ask!(100, 1.0, 1, 1, 1);
    book.add_order(&order).unwrap();

    let updated = order.with_size(udec64!(0.0));
    let result = book.update_order(&updated, &order);

    assert!(matches!(
        result,
        Err(OrderBookError::InvalidOrderSize { order_id: 1, size }) if size == udec64!(0)
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

    let mut book = OrderBook::new();

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
    book.remove_order_by_id(alice_updated.order_id()).unwrap();
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

    let mut book = OrderBook::new();

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

#[test]
fn scenario_size_increase_loses_priority() {
    // Scenario: Three orders at same price, middle one increases size.
    //
    // Initial: [A, B, C] at price 100
    // B increases size: B loses priority
    // Final: [A, C, B]

    let mut book = OrderBook::new();
    let a = ask!(100, 1.0, 1, 1, 1);
    let b = ask!(100, 2.0, 2, 2, 2);
    let c = ask!(100, 3.0, 3, 3, 3);

    book.add_order(&a).unwrap();
    book.add_order(&b).unwrap();
    book.add_order(&c).unwrap();

    assert_fifo!(book, ask @ 100 => [1, 2, 3]);

    // B increases size: 2.0 -> 2.5
    let b_updated = b.with_size(udec64!(2.5));
    book.move_to_back(&b_updated, &b).unwrap();

    assert_fifo!(book, ask @ 100 => [1, 3, 2]);
    assert_level!(book, ask @ 100 => (6.5, 3)); // 1.0 + 3.0 + 2.5
}

#[test]
fn scenario_size_decrease_keeps_priority() {
    // Scenario: Three orders at same price, middle one decreases size.
    //
    // Initial: [A, B, C] at price 100
    // B decreases size: B keeps priority
    // Final: [A, B, C]

    let mut book = OrderBook::new();
    let a = ask!(100, 1.0, 1, 1, 1);
    let b = ask!(100, 2.0, 2, 2, 2);
    let c = ask!(100, 3.0, 3, 3, 3);

    book.add_order(&a).unwrap();
    book.add_order(&b).unwrap();
    book.add_order(&c).unwrap();

    assert_fifo!(book, ask @ 100 => [1, 2, 3]);

    // B decreases size: 2.0 -> 1.5 (use update_order, not move_to_back)
    let b_updated = b.with_size(udec64!(1.5));
    book.update_order(&b_updated, &b).unwrap();

    assert_fifo!(book, ask @ 100 => [1, 2, 3]); // Order preserved
    assert_level!(book, ask @ 100 => (5.5, 3)); // 1.0 + 1.5 + 3.0
}

// ============================================================================
// SNAPSHOT RECONSTRUCTION TESTS
// ============================================================================

#[test]
fn snapshot_single_order() {
    // Reconstruct a single order from snapshot.
    let mut book = OrderBook::new();
    let order = ask!(100, 1.0, 1, 1, 1);

    book.add_orders_from_snapshot(&[order]).unwrap();

    assert_eq!(book.total_orders(), 1);
    assert_order!(book, 1 => { price: 100, size: 1.0, account_id: 1 });
    assert_best_ask!(book, 100, 1.0);
}

#[test]
fn snapshot_multiple_orders_same_level() {
    // Reconstruct multiple orders at the same price level.
    // The FIFO order should be determined by the linked list pointers.
    let mut book = OrderBook::new();

    // Create orders with linked list pointers
    let order1 = Order::for_l3_testing_with_links(
        types::OrderType::OpenShort,
        udec64!(100),
        udec64!(1.0),
        1,
        1,
        1,
        None,    // prev
        Some(2), // next
    );
    let order2 = Order::for_l3_testing_with_links(
        types::OrderType::OpenShort,
        udec64!(100),
        udec64!(2.0),
        2,
        2,
        2,
        Some(1), // prev
        Some(3), // next
    );
    let order3 = Order::for_l3_testing_with_links(
        types::OrderType::OpenShort,
        udec64!(100),
        udec64!(3.0),
        3,
        3,
        3,
        Some(2), // prev
        None,    // next
    );

    // Add in shuffled order - linked list should still be reconstructed correctly
    book.add_orders_from_snapshot(&[order2, order3, order1]).unwrap();

    assert_eq!(book.total_orders(), 3);
    assert_level!(book, ask @ 100 => (6.0, 3));
    assert_fifo!(book, ask @ 100 => [1, 2, 3]); // FIFO from linked list
}

#[test]
fn snapshot_multiple_levels() {
    // Reconstruct orders at multiple price levels.
    let mut book = OrderBook::new();

    let orders = [
        ask!(100, 1.0, 1, 1, 1),
        ask!(110, 2.0, 2, 2, 2),
        bid!(90, 1.5, 3, 3, 3),
        bid!(80, 2.5, 4, 4, 4),
    ];

    book.add_orders_from_snapshot(&orders).unwrap();

    assert_eq!(book.total_orders(), 4);
    assert_best_ask!(book, 100, 1.0);
    assert_best_bid!(book, 90, 1.5);
}

// ============================================================================
// LINKED LIST INTEGRITY TESTS
// ============================================================================

#[test]
fn linked_list_head_tail_single_order() {
    // Single order: head and tail point to the same order.
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();

    let level = book.ask_level(udec64!(100)).unwrap();
    assert!(level.head().is_some());
    assert!(level.tail().is_some());
    assert_eq!(level.head(), level.tail());
}

#[test]
fn linked_list_head_tail_multiple_orders() {
    // Multiple orders: head is first, tail is last.
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 2.0, 2, 2, 2)).unwrap();
    book.add_order(&ask!(100, 3.0, 3, 3, 3)).unwrap();

    let level = book.ask_level(udec64!(100)).unwrap();
    let head = level.head().unwrap();
    let tail = level.tail().unwrap();

    // Head should be order 1, tail should be order 3
    let head_order = book.all_orders().get(head).unwrap();
    let tail_order = book.all_orders().get(tail).unwrap();
    assert_eq!(head_order.order_id(), 1);
    assert_eq!(tail_order.order_id(), 3);

    // Head has no prev, tail has no next
    assert!(head_order.prev().is_none());
    assert!(tail_order.next().is_none());
}

#[test]
fn linked_list_remove_head() {
    // Removing head order updates the level head.
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 2.0, 2, 2, 2)).unwrap();

    book.remove_order_by_id(1).unwrap();

    let level = book.ask_level(udec64!(100)).unwrap();
    let head = level.head().unwrap();
    let head_order = book.all_orders().get(head).unwrap();
    assert_eq!(head_order.order_id(), 2);
    assert!(head_order.prev().is_none()); // New head has no prev
}

#[test]
fn linked_list_remove_tail() {
    // Removing tail order updates the level tail.
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 2.0, 2, 2, 2)).unwrap();

    book.remove_order_by_id(2).unwrap();

    let level = book.ask_level(udec64!(100)).unwrap();
    let tail = level.tail().unwrap();
    let tail_order = book.all_orders().get(tail).unwrap();
    assert_eq!(tail_order.order_id(), 1);
    assert!(tail_order.next().is_none()); // New tail has no next
}

#[test]
fn linked_list_remove_middle() {
    // Removing middle order links neighbors correctly.
    let mut book = OrderBook::new();
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&ask!(100, 2.0, 2, 2, 2)).unwrap();
    book.add_order(&ask!(100, 3.0, 3, 3, 3)).unwrap();

    book.remove_order_by_id(2).unwrap();

    // FIFO should now be [1, 3]
    assert_fifo!(book, ask @ 100 => [1, 3]);

    // Check links
    let level = book.ask_level(udec64!(100)).unwrap();
    let head = level.head().unwrap();
    let tail = level.tail().unwrap();
    let head_order = book.all_orders().get(head).unwrap();
    let tail_order = book.all_orders().get(tail).unwrap();

    assert_eq!(head_order.order_id(), 1);
    assert_eq!(tail_order.order_id(), 3);
    assert_eq!(head_order.next(), Some(tail)); // 1's next is 3
    assert_eq!(tail_order.prev(), Some(head)); // 3's prev is 1
}

// ============================================================================
// ADDITIONAL EDGE CASE TESTS
// ============================================================================

#[test]
fn move_to_back_already_at_back() {
    // Order already at back of queue just updates data, no relink needed.
    let mut book = OrderBook::new();
    let order1 = ask!(100, 1.0, 1, 1, 1);
    let order2 = ask!(100, 2.0, 2, 2, 2);
    book.add_order(&order1).unwrap();
    book.add_order(&order2).unwrap();

    assert_fifo!(book, ask @ 100 => [1, 2]);

    // Move order 2 (already at back) to back with size increase
    let order2_updated = order2.with_size(udec64!(3.0));
    book.move_to_back(&order2_updated, &order2).unwrap();

    // FIFO unchanged, size updated
    assert_fifo!(book, ask @ 100 => [1, 2]);
    assert_order!(book, 2 => { price: 100, size: 3.0, account_id: 2 });
    assert_level!(book, ask @ 100 => (4.0, 2)); // 1.0 + 3.0
}

#[test]
fn move_to_back_single_order() {
    // Single order move_to_back is a no-op relink (just update data).
    let mut book = OrderBook::new();
    let order = ask!(100, 1.0, 1, 1, 1);
    book.add_order(&order).unwrap();

    let order_updated = order.with_size(udec64!(2.0));
    book.move_to_back(&order_updated, &order).unwrap();

    assert_fifo!(book, ask @ 100 => [1]);
    assert_order!(book, 1 => { price: 100, size: 2.0, account_id: 1 });
}

#[test]
fn snapshot_orphaned_orders_no_links() {
    // Snapshot with orders that have no linked list pointers
    // (e.g., each order is alone in its price level, or data is incomplete).
    let mut book = OrderBook::new();

    // Orders at same price but with no explicit links (simulates incomplete data)
    let order1 = Order::for_l3_testing_with_links(
        types::OrderType::OpenShort,
        udec64!(100),
        udec64!(1.0),
        1,
        1,
        1,
        None, // no prev
        None, // no next
    );
    let order2 = Order::for_l3_testing_with_links(
        types::OrderType::OpenShort,
        udec64!(100),
        udec64!(2.0),
        2,
        2,
        2,
        None, // no prev
        None, // no next
    );

    book.add_orders_from_snapshot(&[order1, order2]).unwrap();

    // Both orders should be in the book
    assert_eq!(book.total_orders(), 2);
    assert_level!(book, ask @ 100 => (3.0, 2));

    // When links are missing, we still have valid head/tail (at least one order is head, one is tail)
    let level = book.ask_level(udec64!(100)).unwrap();
    assert!(level.head().is_some());
    assert!(level.tail().is_some());
}

#[test]
fn interleaved_ask_bid_operations() {
    // Test interleaved operations on both sides of the book.
    let mut book = OrderBook::new();

    // Add asks and bids interleaved
    book.add_order(&ask!(100, 1.0, 1, 1, 1)).unwrap();
    book.add_order(&bid!(90, 1.5, 2, 2, 2)).unwrap();
    book.add_order(&ask!(100, 2.0, 3, 3, 3)).unwrap();
    book.add_order(&bid!(90, 2.5, 4, 4, 4)).unwrap();

    assert_level!(book, ask @ 100 => (3.0, 2));
    assert_level!(book, bid @ 90 => (4.0, 2));
    assert_fifo!(book, ask @ 100 => [1, 3]);
    assert_fifo!(book, bid @ 90 => [2, 4]);

    // Remove from both sides
    book.remove_order_by_id(1).unwrap();
    book.remove_order_by_id(2).unwrap();

    assert_level!(book, ask @ 100 => (2.0, 1));
    assert_level!(book, bid @ 90 => (2.5, 1));
    assert_fifo!(book, ask @ 100 => [3]);
    assert_fifo!(book, bid @ 90 => [4]);
}

#[test]
fn move_nonexistent_order() {
    // Move to back on nonexistent order returns error.
    let mut book = OrderBook::new();
    let order = ask!(100, 1.0, 1, 99, 1); // order_id = 99, not in book

    let result = book.move_to_back(&order, &order);
    assert!(matches!(result, Err(OrderBookError::OrderNotFound { order_id: 99 })));
}
