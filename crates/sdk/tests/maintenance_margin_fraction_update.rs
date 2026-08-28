use fastnum::{udec64, udec128};
use perpl_sdk::{
    state::{PositionEvent, PositionEventType, PositionType, StateEvents},
    testing,
    types::{self, RequestType::*},
};

/// Tests that a `MaintenanceMarginFractionUpdated` event re-applies the new
/// maintenance margin to every tracked position on the affected perpetual,
/// updating its maintenance margin requirement and, consequently, its
/// liquidation price (regression for A-1741).
#[tokio::test]
async fn test_maintenance_margin_fraction_update() {
    let exchange = testing::TestExchange::new().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;
    let btc_perp = exchange.btc_perp().await;

    let o = async |acc, r, ot, p, s| {
        _ = btc_perp
            .order(
                acc,
                types::OrderRequest::new(
                    r,
                    btc_perp.id,
                    ot,
                    None,
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
                    1000,
                ),
            )
            .await
            .get_receipt()
            .await
            .unwrap();
    };

    // Cross a maker short against a taker long so both accounts hold an open
    // 0.1 BTC position at an entry price of 100000.
    o(maker.id, 1, OpenShort, udec64!(100000), udec64!(1)).await;
    o(taker.id, 2, OpenLong, udec64!(100000), udec64!(0.1)).await;

    let (indexer, mut state) = testing::Indexer::new(&exchange).await;

    // Capture the pre-update state. The perpetual is configured with a
    // maintenance margin of 20, so each position's maintenance margin
    // requirement is entry * size / mm = 100000 * 0.1 / 20 = 500.
    let (maker_liq_before, taker_liq_before) = {
        let snapshot = state.snapshot();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        assert_eq!(perp.maintenance_margin(), udec64!(20));

        let maker_pos = snapshot
            .accounts()
            .get(&maker.id)
            .unwrap()
            .positions()
            .get(&btc_perp.id)
            .unwrap();
        let taker_pos = snapshot
            .accounts()
            .get(&taker.id)
            .unwrap()
            .positions()
            .get(&btc_perp.id)
            .unwrap();

        assert_eq!(maker_pos.r#type(), PositionType::Short);
        assert_eq!(taker_pos.r#type(), PositionType::Long);
        assert_eq!(maker_pos.size(), udec64!(0.1));
        assert_eq!(taker_pos.size(), udec64!(0.1));
        assert_eq!(maker_pos.maintenance_margin_requirement(), udec128!(500));
        assert_eq!(taker_pos.maintenance_margin_requirement(), udec128!(500));

        (maker_pos.liquidation_price(), taker_pos.liquidation_price())
    };

    // Start processing events on top of the snapshot.
    tokio::spawn(indexer.run(tokio::time::sleep));

    // Double the maintenance margin (20 -> 40), which halves the maintenance
    // margin requirement of every tracked position to 250. The contract only
    // allows raising the on-chain maintenance margin fraction, never lowering
    // it, so the update must move in this direction.
    let receipt = btc_perp
        .set_maintenance_margin(udec64!(40))
        .await
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "set_maintenance_margin transaction reverted");

    // Wait until the maintenance margin update has been applied to positions.
    let mut updated_accounts = 0;
    while let Some(block_events) = state.next_state_events().await {
        for event in block_events.events().iter().flat_map(|e| e.event()) {
            if let StateEvents::Position(PositionEvent {
                perpetual_id: 16,
                r#type: PositionEventType::MaintenanceMarginUpdated(mmr),
                ..
            }) = event
            {
                // The new requirement is entry * size / mm = 100000 * 0.1 / 40.
                assert_eq!(*mmr, udec128!(250));
                updated_accounts += 1;
            }
        }
        if updated_accounts == 2 {
            break;
        }
        assert!(
            block_events.instant().block_number() < 100,
            "position maintenance margin was not updated",
        );
    }
    assert_eq!(updated_accounts, 2, "both tracked positions must be updated");

    // The updated snapshot must reflect the new maintenance margin on the
    // perpetual and the recomputed liquidation prices on both positions.
    let snapshot = state.snapshot();
    let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
    assert_eq!(perp.maintenance_margin(), udec64!(40));

    let maker_pos = snapshot
        .accounts()
        .get(&maker.id)
        .unwrap()
        .positions()
        .get(&btc_perp.id)
        .unwrap();
    let taker_pos = snapshot
        .accounts()
        .get(&taker.id)
        .unwrap()
        .positions()
        .get(&btc_perp.id)
        .unwrap();

    assert_eq!(maker_pos.maintenance_margin_requirement(), udec128!(250));
    assert_eq!(taker_pos.maintenance_margin_requirement(), udec128!(250));

    // Lowering the requirement widens the gap to liquidation: each price shifts
    // by (old_mmr - new_mmr) / size = 250 / 0.1 = 2500 away from entry — up for
    // the short (liquidates above entry) and down for the long.
    assert_eq!(maker_pos.liquidation_price(), maker_liq_before + udec64!(2500));
    assert_eq!(taker_pos.liquidation_price(), taker_liq_before - udec64!(2500));
}
