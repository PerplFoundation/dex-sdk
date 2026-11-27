use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use fastnum::{UD64, udec64, udec128};
use perpl_sdk::{state, testing, types};

/// Price levels quoted on each side, an ask and a bid per level: every order
/// ID the book has, less the null one.
const LEVELS: usize = u16::MAX as usize / 2;

/// Price levels each maker quotes: 150 orders, the most the exchange lets an
/// account rest on one perpetual (`_MAX_ACCOUNT_PERP_ORDERS`, contract
/// v1.1.7.6+).
const LEVELS_PER_MAKER: usize = 75;

/// Maker accounts it takes to fill the book at that cap.
const MAKERS: usize = LEVELS.div_ceil(LEVELS_PER_MAKER);

/// Tests the creation of exchange snapshot when perpetual order book is full:
/// every one of its 65535 order IDs taken, read back across the ID bitmap and
/// as many `getOrderV2` batches as that takes.
#[tokio::test]
async fn test_full_book_snapshot() {
    let exchange = testing::TestExchange::new().await;
    let btc_perp = exchange.btc_perp().await;
    let makers = exchange.accounts(0..MAKERS, 1_000_000).await;
    let taker = exchange.account(MAKERS, 100_000).await;

    // Fill the book: 100 orders of 0.001 at every price level, 100 apart on
    // either side of 100000, each maker at its cap (the last one short of it)
    let started_at = Instant::now();
    let size = udec64!(0.001);
    let leverage = udec64!(10);
    let mut pending_txs = vec![];
    for (m, maker) in makers.iter().enumerate() {
        let levels = (m * LEVELS_PER_MAKER..((m + 1) * LEVELS_PER_MAKER).min(LEVELS))
            .map(|offs| {
                let offs = offs as u64;
                (UD64::from(100100u64 + offs / 100 * 100), UD64::from(99900u64 - offs / 100 * 100))
            })
            .collect::<Vec<_>>();
        for (chunk, levels) in levels.chunks(50).enumerate() {
            let orders = levels
                .iter()
                .enumerate()
                .flat_map(|(level, (ask, bid))| {
                    let request_id = ((m * LEVELS_PER_MAKER + chunk * 50 + level) * 2) as u64;
                    vec![
                        types::OrderRequest::new(
                            request_id,
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
                            1000,
                        ),
                        types::OrderRequest::new(
                            request_id + 1,
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
                            1000,
                        ),
                    ]
                })
                .collect();
            pending_txs.push(btc_perp.orders(maker.id, orders).await);
        }
        if m % 100 == 0 {
            pending_txs.push(btc_perp.set_mark_price(udec64!(100000)).await);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let receipts =
        futures::future::join_all(pending_txs.into_iter().map(|ptx| ptx.get_receipt())).await;
    for (i, r) in receipts.iter().enumerate() {
        assert!(r.as_ref().unwrap().status(), "#{}: {:#?}", i, r);
    }
    println!("book filled in: {:?}", started_at.elapsed());

    // Do some trades
    btc_perp
        .set_mark_price(udec64!(100000))
        .await
        .get_receipt()
        .await
        .unwrap();
    let receipt = btc_perp
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
                    1000,
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
                    1000,
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
                    1000,
                ),
            ],
        )
        .await
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "{:#?}", receipt);

    // Take the snapshot
    let started_at = Instant::now();
    let snap = state::SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
        .with_accounts(vec![
            types::AccountAddressOrID::Address(makers[0].address),
            types::AccountAddressOrID::Address(taker.address),
        ])
        .build()
        .await
        .unwrap();
    println!("snapshot taken in: {:?}", started_at.elapsed());

    let traded_at = receipt.block_number.expect("mined");
    assert!(
        snap.instant().block_number() >= traded_at,
        "snapshot at block {} predates the trades at {}",
        snap.instant().block_number(),
        traded_at,
    );
    assert!(!snap.is_halted());
    assert_eq!(snap.perpetuals().len(), 1);
    assert_eq!(snap.accounts().len(), 2);

    let perp = snap.perpetuals().get(&btc_perp.id).unwrap();
    assert!(perp.instant().block_number() >= traded_at);
    assert_eq!(perp.id(), btc_perp.id);
    assert_eq!(perp.name(), "BTC".to_string());
    assert_eq!(perp.symbol(), "BTC".to_string());
    assert!(!perp.is_paused());
    // Base (tier 0) rates of the exchange-wide default schedule, seeded at
    // deployment by `Exchange::_seedDefaultFeeSchedule` - a listing does not set
    // its own fees
    assert_eq!(perp.maker_fee(), udec64!(0.000045));
    assert_eq!(perp.taker_fee(), udec64!(0.000345));
    assert_eq!(perp.initial_margin(), udec64!(10));
    assert_eq!(perp.maintenance_margin(), udec64!(20));
    assert_eq!(perp.last_price(), udec64!(99900));
    assert_eq!(perp.mark_price(), udec64!(100000));
    assert_eq!(perp.funding_start_block(), 8571);
    assert_eq!(perp.open_interest(), udec128!(0.09));

    // 65534 resting orders, less the 100 asks of the first level the taker
    // took and the 10 bids its close sold into
    assert_eq!(perp.total_orders(), LEVELS * 2 - 110);
    // Verify all orders belong to maker accounts with expected size
    let maker_ids = makers.iter().map(|m| m.id).collect::<HashSet<_>>();
    let all_asks_valid = perp
        .l3_book()
        .ask_orders()
        .all(|o| maker_ids.contains(&o.account_id()) && o.size() == udec64!(0.001));
    let all_bids_valid = perp
        .l3_book()
        .bid_orders()
        .all(|o| maker_ids.contains(&o.account_id()) && o.size() == udec64!(0.001));
    assert!(all_asks_valid && all_bids_valid);
    // ...and every maker is at the cap, less what the taker took from it
    for maker in &makers {
        let resting = perp
            .l3_book()
            .ask_orders()
            .chain(perp.l3_book().bid_orders())
            .filter(|o| o.account_id() == maker.id)
            .count();
        assert!(resting <= LEVELS_PER_MAKER * 2, "maker {} rests {resting}", maker.id);
    }

    assert_eq!(perp.l3_book().best_ask(), Some((udec64!(100200), udec64!(0.1))));
    assert_eq!(perp.l3_book().best_bid(), Some((udec64!(99900), udec64!(0.09))));

    // Ten full levels of 0.1 from 100200 up
    assert_eq!(
        perp.l3_book().ask_impact(udec64!(1)),
        Some((udec64!(101100), udec64!(1), udec64!(100650)))
    );
    // 0.09 at 99900, nine full levels down to 99000, and 0.01 of 98900:
    // 8991 + 89460 + 989
    assert_eq!(
        perp.l3_book().bid_impact(udec64!(1)),
        Some((udec64!(98900), udec64!(1), udec64!(99440)))
    );
}
