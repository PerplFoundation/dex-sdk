use std::{pin::pin, sync::Arc};

use dex_sdk::{
    state::{
        self, AccountEvent, AccountEventType, OrderEvent, OrderEventType, PositionEvent,
        PositionEventType,
    },
    stream, testing,
    types::{self, RequestType::*},
};
use fastnum::{udec64, udec128};
use futures::StreamExt;
use tokio::sync::{RwLock, mpsc};

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

    // Snapshot
    let snapshot = Arc::new(RwLock::new(
        state::SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
            .with_all_positions()
            .build()
            .await
            .unwrap(),
    ));

    assert_eq!(snapshot.read().await.perpetuals().len(), 1);
    assert_eq!(snapshot.read().await.accounts().len(), 2);

    {
        let snapshot = snapshot.read().await;
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

        assert_eq!(perp.orders().len(), 1);

        let order = perp.orders().get(&1).unwrap();
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

    // Spin up event stream consumption in background to make sure stream is
    // actually following the tip of the chain
    let (results_tx, mut results_rx) = mpsc::unbounded_channel();
    tokio::spawn({
        let (chain, provider, snapshot) = (
            exchange.chain().clone(),
            exchange.provider.clone(),
            snapshot.clone(),
        );
        async move {
            let mut stream = pin!(
                stream::raw(
                    &chain,
                    provider,
                    snapshot.read().await.instant(),
                    tokio::time::sleep
                )
                .take(20)
            );
            while let Some(batch) = stream.next().await {
                let batch = batch.unwrap();
                let result = snapshot.write().await.apply_events(&batch).unwrap();
                results_tx.send(result).unwrap();
            }
        }
    });

    // A bit more activity
    o(maker.id, 10, Some(1), Change, udec64!(100100), udec64!(1)).await;
    o(taker.id, 11, None, OpenLong, udec64!(100100), udec64!(0.1)).await;
    o(maker.id, 12, Some(1), Cancel, udec64!(0), udec64!(0)).await;

    o(maker.id, 20, None, OpenLong, udec64!(100100), udec64!(1)).await;
    o(taker.id, 21, None, CloseLong, udec64!(100100), udec64!(0.2)).await;

    // Collect and (partially) validate produced events
    while let Some(block_events) = results_rx.recv().await {
        if let Some(block_events) = block_events {
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
                        order_id: Some(1),
                        r#type:
                            OrderEventType::Updated {
                                price,
                                size,
                                expiry_block,
                            },
                    }) => {
                        assert_eq!(*price, Some(udec64!(100100)));
                        assert_eq!(*size, Some(udec64!(1)));
                        assert_eq!(*expiry_block, None);
                    }
                    state::StateEvents::Order(OrderEvent {
                        perpetual_id: 16,
                        account_id: 1,
                        request_id: Some(11),
                        order_id: Some(1),
                        r#type:
                            OrderEventType::Filled {
                                fill_price,
                                fill_size,
                                fee,
                                is_maker,
                            },
                    }) => {
                        assert_eq!(*fill_price, udec64!(100100));
                        assert_eq!(*fill_size, udec64!(0.1));
                        assert_eq!(*fee, udec64!(1.001));
                        assert_eq!(*is_maker, true);
                    }

                    state::StateEvents::Position(PositionEvent {
                        perpetual_id: 16,
                        account_id: 2,
                        request_id: Some(11),
                        r#type:
                            PositionEventType::Increased {
                                entry_price,
                                prev_size,
                                new_size,
                                deposit,
                            },
                    }) => {
                        assert_eq!(*entry_price, udec64!(100050));
                        assert_eq!(*prev_size, udec64!(0.1));
                        assert_eq!(*new_size, udec64!(0.2));
                        assert_eq!(*deposit, udec128!(2002));
                    }

                    _ => (),
                }
            }
        }
    }

    // Validate updated snapshot
    {
        let snapshot = snapshot.read().await;
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        assert_eq!(perp.last_price(), udec64!(100100));
        assert_eq!(perp.open_interest(), udec128!(0));

        assert_eq!(perp.orders().len(), 1);

        let order = perp.orders().get(&1).unwrap();
        assert_eq!(order.r#type(), types::OrderType::OpenLong);
        assert_eq!(order.price(), udec64!(100100));
        assert_eq!(order.size(), udec64!(0.8));

        let maker = snapshot.accounts().get(&maker.id).unwrap();
        assert_eq!(maker.positions().len(), 0);

        let taker = snapshot.accounts().get(&taker.id).unwrap();
        assert_eq!(taker.positions().len(), 0);
    }
}
