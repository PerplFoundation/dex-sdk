use std::num::NonZeroU16;

use fastnum::{udec64, udec128};
use perpl_sdk::{
    state::{
        self, AccountEvent, AccountEventType, OrderEvent, OrderEventType, PositionEvent,
        PositionEventType,
    },
    testing,
    types::{self, RequestType::*},
};

fn oid(n: u16) -> types::OrderId { NonZeroU16::new(n).expect("test order id must be non-zero") }

/// Tests the creation of initial exchange snapshot followed by
/// updating it with real-time events.
#[tokio::test]
async fn test_snapshot_and_events() {
    let exchange = testing::TestExchange::new().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;
    let btc_perp = exchange.btc_perp().await;

    let o = async |acc, r, oid, ot, p, s| {
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
                    None,
                    false,
                    false,
                    false,
                    None,
                    udec64!(10),
                    None,
                    None,
                ),
            )
            .await
            .get_receipt()
            .await
            .unwrap();
    };

    // Some initial state
    o(maker.id, 1, None, OpenShort, udec64!(100000), udec64!(1)).await;
    o(taker.id, 2, None, OpenLong, udec64!(100000), udec64!(0.1)).await;

    // Take snapshot
    let (indexer, mut state) = testing::Indexer::new(&exchange).await;

    assert_eq!(state.snapshot().perpetuals().len(), 1);
    assert_eq!(state.snapshot().accounts().len(), 2);

    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        assert_eq!(perp.id(), btc_perp.id);
        assert_eq!(perp.name(), "BTC".to_string());
        assert_eq!(perp.symbol(), "BTC".to_string());
        assert_eq!(perp.is_paused(), false);
        assert_eq!(perp.maker_fee(), udec64!(0.00010));
        assert_eq!(perp.taker_fee(), udec64!(0.00035));
        assert_eq!(perp.initial_margin(), udec64!(10));
        assert_eq!(perp.maintenance_margin(), udec64!(20));
        assert_eq!(perp.last_price(), udec64!(100000));
        assert_eq!(perp.mark_price(), udec64!(100000));
        assert_eq!(perp.funding_start_block(), 8571);
        assert_eq!(perp.open_interest(), udec128!(0.1));

        assert_eq!(perp.total_orders(), 1);

        let order = perp.get_order(oid(1)).unwrap();
        assert_eq!(order.r#type(), types::OrderType::OpenShort);
        assert_eq!(order.price(), udec64!(100000));
        assert_eq!(order.size(), udec64!(0.9));
        assert_eq!(order.placed_size(), None);

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

    // A bit more activity
    o(maker.id, 10, Some(oid(1)), Change, udec64!(100100), udec64!(1)).await;
    o(taker.id, 11, None, OpenLong, udec64!(100100), udec64!(0.1)).await;
    o(maker.id, 12, Some(oid(1)), Cancel, udec64!(0), udec64!(0)).await;

    o(maker.id, 20, None, OpenLong, udec64!(100100), udec64!(1)).await;
    o(taker.id, 21, None, CloseLong, udec64!(100100), udec64!(0.2)).await;

    // Collect and (partially) validate produced events
    while let Some(block_events) = state.next_state_events().await {
        for event in block_events.events().iter().map(|e| e.event()).flatten() {
            match event {
                state::StateEvents::Account(AccountEvent {
                    account_id: 1,
                    request_id: Some(10),
                    r#type: AccountEventType::BalanceUpdated(balance),
                }) => assert_eq!(*balance, udec128!(998998.9)),
                state::StateEvents::Account(AccountEvent {
                    account_id: 1,
                    request_id: Some(11),
                    r#type: AccountEventType::BalanceUpdated(balance),
                }) => assert_eq!(*balance, udec128!(997995.899)),
                state::StateEvents::Account(AccountEvent {
                    account_id: 2,
                    request_id: Some(11),
                    r#type: AccountEventType::BalanceUpdated(balance),
                }) => assert_eq!(*balance, udec128!(97990.9965)),

                state::StateEvents::Order(OrderEvent {
                    perpetual_id: 16,
                    account_id: 1,
                    request_id: Some(10),
                    order_id: Some(order_id),
                    r#type: OrderEventType::Updated { price, size, expiry_block },
                }) if *order_id == oid(1) => {
                    assert_eq!(*price, Some(udec64!(100100)));
                    assert_eq!(*size, Some(udec64!(1)));
                    assert_eq!(*expiry_block, None);
                },
                state::StateEvents::Order(OrderEvent {
                    perpetual_id: 16,
                    account_id: 1,
                    request_id: Some(11),
                    order_id: Some(order_id),
                    r#type: OrderEventType::Filled { fill_price, fill_size, fee, is_maker },
                }) if *order_id == oid(1) => {
                    assert_eq!(*fill_price, udec64!(100100));
                    assert_eq!(*fill_size, udec64!(0.1));
                    assert_eq!(*fee, udec64!(1.001));
                    assert_eq!(*is_maker, true);
                },

                state::StateEvents::Position(PositionEvent {
                    perpetual_id: 16,
                    account_id: 2,
                    request_id: Some(11),
                    r#type:
                        PositionEventType::Increased { entry_price, prev_size, new_size, deposit },
                }) => {
                    assert_eq!(*entry_price, udec64!(100050));
                    assert_eq!(*prev_size, udec64!(0.1));
                    assert_eq!(*new_size, udec64!(0.2));
                    assert_eq!(*deposit, udec128!(2002));
                },

                _ => (),
            }
        }

        if state.request_id_seen(21) {
            break;
        }
    }

    // Validate updated snapshot
    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        assert_eq!(perp.last_price(), udec64!(100100));
        assert_eq!(perp.open_interest(), udec128!(0));

        assert_eq!(perp.total_orders(), 1);

        let order = perp.get_order(oid(1)).unwrap();
        assert_eq!(order.r#type(), types::OrderType::OpenLong);
        assert_eq!(order.price(), udec64!(100100));
        assert_eq!(order.size(), udec64!(0.8));
        assert_eq!(order.placed_size(), Some(udec64!(1)));
        assert_eq!(order.filled_size(), Some(udec64!(0.2)));

        let maker = snapshot.accounts().get(&maker.id).unwrap();
        assert_eq!(maker.positions().len(), 0);

        let taker = snapshot.accounts().get(&taker.id).unwrap();
        assert_eq!(taker.positions().len(), 0);
    }
}
