use std::{num::NonZeroU16, time::Duration};

use fastnum::udec64;
use perpl_sdk::{
    state, testing,
    types::{self, RequestType::*},
};

fn oid(n: u16) -> types::OrderId { NonZeroU16::new(n).expect("test order id must be non-zero") }

/// Tests the order book state tracking on real-time events.
#[tokio::test]
async fn test_order_book() {
    let exchange = testing::TestExchange::new().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;
    let btc_perp = exchange.btc_perp().await;

    let o = async |acc, r, oid, ot, p, s, exp| {
        _ = btc_perp
            .order(
                acc,
                types::OrderRequest::new(
                    r,
                    btc_perp.id,
                    ot,
                    oid,
                    p,
                    s,
                    exp,
                    false,
                    false,
                    false,
                    None,
                    udec64!(10),
                    None,
                    None,
                    1000,
                ),
            )
            .await
            .get_receipt()
            .await
            .unwrap()
    };

    // Some initial state
    o(maker.id, 1, None, OpenShort, udec64!(100000), udec64!(1), None).await; // #1
    o(taker.id, 2, None, OpenLong, udec64!(100000), udec64!(0.1), None).await; // immediate

    // Take initial snapshot
    let (indexer, mut state) = testing::Indexer::new(&exchange).await;

    {
        let snapshot = state.snapshot().clone();

        assert_eq!(snapshot.perpetuals().len(), 1);
        assert_eq!(snapshot.accounts().len(), 2);

        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        let book = perp.l3_book();

        assert_eq!(book.best_ask(), Some((udec64!(100000), udec64!(0.9))));
        assert_eq!(book.best_bid(), None);

        let ask_level = book.ask_level(udec64!(100000)).unwrap();
        assert_eq!(ask_level.num_orders(), 1);
        assert_eq!(ask_level.size(), udec64!(0.9));
        assert_eq!(ask_level.is_empty(), false);

        let order = perp.get_order(oid(1)).unwrap();
        assert_eq!(order.r#type(), types::OrderType::OpenShort);
        assert_eq!(order.price(), udec64!(100000));
        assert_eq!(order.size(), udec64!(0.9));

        let maker = snapshot.accounts().get(&maker.id).unwrap();
        assert_eq!(maker.positions().len(), 1);

        let maker_pos = maker.positions().get(&btc_perp.id).unwrap();
        assert_eq!(maker_pos.r#type(), state::PositionType::Short);
        assert_eq!(maker_pos.entry_price(), udec64!(100000));
        assert_eq!(maker_pos.size(), udec64!(0.1));

        let taker = snapshot.accounts().get(&taker.id).unwrap();
        assert_eq!(taker.positions().len(), 1);

        let taker_pos = taker.positions().get(&btc_perp.id).unwrap();
        assert_eq!(taker_pos.r#type(), state::PositionType::Long);
        assert_eq!(taker_pos.entry_price(), udec64!(100000));
        assert_eq!(taker_pos.size(), udec64!(0.1));
    }

    // Start processing events
    tokio::spawn(indexer.run(tokio::time::sleep));

    // A few more orders
    o(maker.id, 10, None, OpenShort, udec64!(100000), udec64!(2), None).await; // #2
    o(maker.id, 11, None, OpenShort, udec64!(99900), udec64!(1), None).await; // #3
    o(taker.id, 20, None, OpenLong, udec64!(99000), udec64!(0.5), None).await; // #4
    assert!(
        tokio::time::timeout(Duration::from_secs(5), state.wait_for(None, Some(20)))
            .await
            .unwrap()
    );

    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        let book = perp.l3_book();

        assert_eq!(book.best_ask(), Some((udec64!(99900), udec64!(1))));
        assert_eq!(book.best_bid(), Some((udec64!(99000), udec64!(0.5))));

        let ask_level = book.ask_level(udec64!(100000)).unwrap();
        assert_eq!(ask_level.num_orders(), 2);
        assert_eq!(ask_level.size(), udec64!(2.9));
        assert_eq!(ask_level.is_empty(), false);

        let ask_level = book.ask_level(udec64!(99900)).unwrap();
        assert_eq!(ask_level.num_orders(), 1);
        assert_eq!(ask_level.size(), udec64!(1));
        assert_eq!(ask_level.is_empty(), false);

        let bid_level = book.bid_level(udec64!(99000)).unwrap();
        assert_eq!(bid_level.num_orders(), 1);
        assert_eq!(bid_level.size(), udec64!(0.5));
        assert_eq!(bid_level.is_empty(), false);
    }

    // Orders to expire
    o(maker.id, 30, None, OpenShort, udec64!(100000), udec64!(3), Some(50)).await; // #5
    o(maker.id, 31, None, OpenShort, udec64!(99800), udec64!(2), Some(50)).await; // #6
    o(taker.id, 40, None, OpenLong, udec64!(99000), udec64!(0.7), Some(50)).await; // #7
    assert!(
        tokio::time::timeout(Duration::from_secs(5), state.wait_for(None, Some(40)))
            .await
            .unwrap()
    );

    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        let book = perp.l3_book();

        assert_eq!(book.best_ask(), Some((udec64!(99800), udec64!(2))));
        assert_eq!(book.best_bid(), Some((udec64!(99000), udec64!(1.2))));

        let ask_level = book.ask_level(udec64!(100000)).unwrap();
        assert_eq!(ask_level.num_orders(), 3);
        assert_eq!(ask_level.size(), udec64!(5.9));
        assert_eq!(ask_level.is_empty(), false);

        let ask_level = book.ask_level(udec64!(99900)).unwrap();
        assert_eq!(ask_level.num_orders(), 1);
        assert_eq!(ask_level.size(), udec64!(1));
        assert_eq!(ask_level.is_empty(), false);

        let ask_level = book.ask_level(udec64!(99800)).unwrap();
        assert_eq!(ask_level.num_orders(), 1);
        assert_eq!(ask_level.size(), udec64!(2));
        assert_eq!(ask_level.is_empty(), false);

        let bid_level = book.bid_level(udec64!(99000)).unwrap();
        assert_eq!(bid_level.num_orders(), 2);
        assert_eq!(bid_level.size(), udec64!(1.2));
        assert_eq!(bid_level.is_empty(), false);
    }

    // Wait for expiration
    assert!(
        tokio::time::timeout(Duration::from_secs(20), state.wait_for(Some(50), None))
            .await
            .unwrap()
    );

    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        let book = perp.l3_book();

        assert_eq!(book.best_ask(), Some((udec64!(99900), udec64!(1))));
        assert_eq!(book.best_bid(), Some((udec64!(99000), udec64!(0.5))));

        let ask_level = book.ask_level(udec64!(100000)).unwrap();
        assert_eq!(ask_level.num_orders(), 2);
        assert_eq!(ask_level.size(), udec64!(2.9));
        assert_eq!(ask_level.is_empty(), false);

        let ask_level = book.ask_level(udec64!(99900)).unwrap();
        assert_eq!(ask_level.num_orders(), 1);
        assert_eq!(ask_level.size(), udec64!(1));
        assert_eq!(ask_level.is_empty(), false);

        let bid_level = book.bid_level(udec64!(99000)).unwrap();
        assert_eq!(bid_level.num_orders(), 1);
        assert_eq!(bid_level.size(), udec64!(0.5));
        assert_eq!(bid_level.is_empty(), false);
    }

    // Cancel and update expired orders
    o(maker.id, 60, Some(oid(5)), Cancel, udec64!(0), udec64!(0), None).await;
    o(taker.id, 61, Some(oid(7)), Change, udec64!(99000), udec64!(1.3), Some(100)).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), state.wait_for(None, Some(61)))
            .await
            .unwrap()
    );

    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        let book = perp.l3_book();

        assert_eq!(book.best_ask(), Some((udec64!(99900), udec64!(1))));
        assert_eq!(book.best_bid(), Some((udec64!(99000), udec64!(1.8))));

        let ask_level = book.ask_level(udec64!(100000)).unwrap();
        assert_eq!(ask_level.num_orders(), 2);
        assert_eq!(ask_level.size(), udec64!(2.9));
        assert_eq!(ask_level.is_empty(), false);

        let ask_level = book.ask_level(udec64!(99900)).unwrap();
        assert_eq!(ask_level.num_orders(), 1);
        assert_eq!(ask_level.size(), udec64!(1));
        assert_eq!(ask_level.is_empty(), false);

        let bid_level = book.bid_level(udec64!(99000)).unwrap();
        assert_eq!(bid_level.num_orders(), 2);
        assert_eq!(bid_level.size(), udec64!(1.8));
        assert_eq!(bid_level.is_empty(), false);
    }

    // Cancel and update active orders
    o(maker.id, 70, Some(oid(2)), Cancel, udec64!(0), udec64!(0), None).await;
    o(taker.id, 71, Some(oid(4)), Change, udec64!(99000), udec64!(0.2), Some(100)).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), state.wait_for(None, Some(71)))
            .await
            .unwrap()
    );

    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        let book = perp.l3_book();

        assert_eq!(book.best_ask(), Some((udec64!(99900), udec64!(1))));
        assert_eq!(book.best_bid(), Some((udec64!(99000), udec64!(1.5))));

        let ask_level = book.ask_level(udec64!(100000)).unwrap();
        assert_eq!(ask_level.num_orders(), 1);
        assert_eq!(ask_level.size(), udec64!(0.9));
        assert_eq!(ask_level.is_empty(), false);

        let ask_level = book.ask_level(udec64!(99900)).unwrap();
        assert_eq!(ask_level.num_orders(), 1);
        assert_eq!(ask_level.size(), udec64!(1));
        assert_eq!(ask_level.is_empty(), false);

        let bid_level = book.bid_level(udec64!(99000)).unwrap();
        assert_eq!(bid_level.num_orders(), 2);
        assert_eq!(bid_level.size(), udec64!(1.5));
        assert_eq!(bid_level.is_empty(), false);
    }
}
