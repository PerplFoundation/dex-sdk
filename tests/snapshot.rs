use std::time::Instant;

use dex_sdk::{state, testing, types};
use fastnum::{UD64, udec64, udec128};

/// Tests the creation of exchange snapshot when perpetual order book is full.
#[tokio::test]
async fn test_full_book_snapshot() {
    let exchange = testing::TestExchange::new().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;
    let btc_perp = exchange.btc_perp().await;

    // Fill the whole book with orders
    let started_at = Instant::now();
    let size = udec64!(0.001);
    let leverage = udec64!(10);
    let mut pending_txs = vec![];
    for (chunk, levels) in (1..32768)
        .map(|offs| (UD64::from(100000u64 + offs), UD64::from(100000u64 - offs)))
        .collect::<Vec<_>>()
        .chunks(50)
        .enumerate()
    {
        let orders = levels
            .into_iter()
            .enumerate()
            .map(|(level, (ask, bid))| {
                vec![
                    types::OrderRequest::new(
                        chunk as u64 * 100 + level as u64,
                        btc_perp.id,
                        types::RequestType::OpenShort,
                        None,
                        *ask,
                        size,
                        None,
                        true,
                        false,
                        false,
                        None,
                        leverage,
                        None,
                        None,
                    ),
                    types::OrderRequest::new(
                        chunk as u64 * 100 + level as u64 + 1,
                        btc_perp.id,
                        types::RequestType::OpenLong,
                        None,
                        *bid,
                        size,
                        None,
                        true,
                        false,
                        false,
                        None,
                        leverage,
                        None,
                        None,
                    ),
                ]
            })
            .flatten()
            .collect();

        pending_txs.push(btc_perp.orders(maker.id, orders).await);
    }
    futures::future::join_all(pending_txs.into_iter().map(|ptx| ptx.get_receipt())).await;
    println!("book filled in: {:?}", started_at.elapsed());

    btc_perp.set_mark_price(udec64!(100000)).await;

    // Do some trades
    btc_perp
        .orders(
            taker.id,
            vec![
                types::OrderRequest::new(
                    1,
                    btc_perp.id,
                    types::RequestType::OpenLong,
                    None,
                    udec64!(101000),
                    udec64!(0.05),
                    None,
                    false,
                    false,
                    false,
                    None,
                    leverage,
                    None,
                    None,
                ),
                types::OrderRequest::new(
                    2,
                    btc_perp.id,
                    types::RequestType::OpenLong,
                    None,
                    udec64!(101000),
                    udec64!(0.05),
                    None,
                    false,
                    false,
                    false,
                    None,
                    leverage,
                    None,
                    None,
                ),
                types::OrderRequest::new(
                    2,
                    btc_perp.id,
                    types::RequestType::CloseLong,
                    None,
                    udec64!(99500),
                    udec64!(0.01),
                    None,
                    false,
                    false,
                    false,
                    None,
                    leverage,
                    None,
                    None,
                ),
            ],
        )
        .await
        .get_receipt()
        .await
        .unwrap();

    // Take the snapshot
    let started_at = Instant::now();
    let snap = state::SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
        .with_accounts(vec![maker.address, taker.address])
        .build()
        .await
        .unwrap();
    println!("snapshot taken in: {:?}", started_at.elapsed());

    assert!(
        snap.instant().block_number() > 180,
        "actual block num: {}",
        snap.instant().block_number()
    );
    assert_eq!(snap.is_halted(), false);
    assert_eq!(snap.perpetuals().len(), 1);
    assert_eq!(snap.accounts().len(), 2);

    let perp = snap.perpetuals().get(&btc_perp.id).unwrap();
    assert!(perp.instant().block_number() > 200);
    assert_eq!(perp.id(), btc_perp.id);
    assert_eq!(perp.name(), "BTC".to_string());
    assert_eq!(perp.symbol(), "BTC".to_string());
    assert_eq!(perp.is_paused(), false);
    assert_eq!(perp.maker_fee(), udec64!(0.00010));
    assert_eq!(perp.taker_fee(), udec64!(0.00035));
    assert_eq!(perp.initial_margin(), udec64!(10));
    assert_eq!(perp.maintenance_margin(), udec64!(20));
    assert_eq!(perp.last_price(), udec64!(99990));
    assert_eq!(perp.mark_price(), udec64!(100000));
    assert_eq!(perp.funding_start_block(), 8571);
    assert_eq!(perp.open_interest(), udec128!(0.09));

    assert_eq!(perp.total_orders(), 65424);
    assert!(perp
        .l2_book()
        .all_orders()
        .values()
        .all(|o| o.account_id() == maker.id && o.size() == udec64!(0.001)));

    assert_eq!(
        perp.l2_book().best_ask(),
        Some((udec64!(100101), udec64!(0.001)))
    );
    assert_eq!(
        perp.l2_book().best_bid(),
        Some((udec64!(99989), udec64!(0.001)))
    );

    assert_eq!(
        perp.l2_book().ask_impact(udec64!(1)),
        Some((udec64!(101100), udec64!(1), udec64!(100600.5)))
    );
    assert_eq!(
        perp.l2_book().bid_impact(udec64!(1)),
        Some((udec64!(98990), udec64!(1), udec64!(99489.5)))
    );
}
