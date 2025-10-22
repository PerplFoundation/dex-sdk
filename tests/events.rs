use std::pin::pin;

use dex_sdk::{
    state, stream, testing,
    types::{self, RequestType::*},
};
use fastnum::{udec64, udec128};
use futures::StreamExt;

/// Tests the creation of initial exchange snapshot followed by
/// updating it with real-time events.
#[tokio::test]
async fn test_snapshot_and_events() {
    let exchange = testing::TestExchange::new().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;
    let btc_perp = exchange.btc_perp().await;

    let o = async |acc, r, oid, ot, p, s| {
        btc_perp
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
    let mut snapshot = state::SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
        .with_accounts(vec![maker.address, taker.address])
        .build()
        .await
        .unwrap();

    assert_eq!(snapshot.perpetuals().len(), 1);
    assert_eq!(snapshot.accounts().len(), 2);

    {
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

    // A bit more activity
    o(maker.id, 10, Some(1), Change, udec64!(100100), udec64!(1)).await;
    o(taker.id, 11, None, OpenLong, udec64!(100100), udec64!(0.1)).await;
    o(maker.id, 12, Some(1), Cancel, udec64!(0), udec64!(0)).await;

    o(maker.id, 20, None, OpenLong, udec64!(100100), udec64!(1)).await;
    o(taker.id, 21, None, CloseLong, udec64!(100100), udec64!(0.2)).await;

    // Consume events and update snapshot
    let chain = exchange.chain();
    let mut stream = pin!(
        stream::raw(
            &chain,
            exchange.provider.clone(),
            snapshot.instant(),
            tokio::time::sleep
        )
        .take(20)
    );

    let mut results = vec![];
    while let Some(batch) = stream.next().await {
        println!("batch: {:?}", batch);
        let batch = batch.unwrap();
        let result = snapshot.apply_events(&batch).unwrap();
        println!("resunt: {:?}", result);
        results.push(result);
    }

    {
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

    // TODO: results validation
}
