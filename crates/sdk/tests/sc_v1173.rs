//! Validates the smart-contract v1.1.7.3b additions (`PositionInfoV2` /
//! `PerpetualInfoV2`) flow correctly through both the `SnapshotBuilder` V2
//! path and the live event stream:
//!
//! - `fundingSumScalingExp` seeded via the `initializeV2` upgrade hook and read
//!   back via `PerpetualInfoV2` on the snapshot path; the funding converter is
//!   rescaled correspondingly. Persistence through subsequent event-stream
//!   updates is also asserted.
//! - `priceResiduePNSQ16` produced by the SC on `PositionIncreasedV2` (averaged
//!   entry across multiple fills) and applied to position state via both the
//!   snapshot path and the streaming path.

use std::time::Duration;

use alloy::primitives::U256;
use fastnum::udec64;
use perpl_sdk::{
    error::DexError,
    state, testing,
    types::{self, RequestType::*},
};

/// One-time V2 upgrade hook: seeds per-perp `fundingSumScalingExp` for every
/// configured perpetual. `setFundingSumScalingExp` rejects writes after a
/// perpetual is first activated; `initializeV2` is the documented path for
/// configuring the V2 scaling state.
async fn initialize_v2_scaling(
    exchange: &testing::TestExchange,
    configs: Vec<(types::PerpetualId, u8)>,
) {
    use perpl_sdk::abi::dex::Exchange::{PerpScalingConfig, ResidueTransfer};
    let configs = configs
        .into_iter()
        .map(|(perp_id, exp)| PerpScalingConfig {
            perpId: U256::from(perp_id),
            fundingSumScalingExp: U256::from(exp),
        })
        .collect::<Vec<_>>();
    let call = exchange
        .exchange
        .initializeV2(configs, Vec::<ResidueTransfer>::new())
        .gas(2_000_000);
    call.clone()
        .call()
        .await
        .map_err::<DexError, _>(|e| DexError::Provider(e.into()))
        .expect("initializeV2 simulated revert");
    let receipt = call
        .send()
        .await
        .map_err::<DexError, _>(|e| DexError::Provider(e.into()))
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "initializeV2 on-chain revert");
}

/// Tests that `fundingSumScalingExp` and `priceResiduePNSQ16` (V2 fields
/// added in smart-contract v1.1.7.3b) are captured correctly by both the
/// snapshot builder and the live event stream.
#[tokio::test]
async fn test_sc_v1173() {
    let exchange = testing::TestExchange::new().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;

    let initial_funding_exp: u8 = 3;
    let btc_perp = exchange.btc_perp().await;
    // `initializeV2` is the one-time V2 upgrade hook that seeds per-perp
    // `fundingSumScalingExp`. `setFundingSumScalingExp` rejects post-activation
    // writes, so the only path to a non-default scaling exp in a test is via
    // this hook.
    initialize_v2_scaling(&exchange, vec![(btc_perp.id, initial_funding_exp)]).await;

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

    // Create an averaged-entry-price position before snapshot to exercise
    // the V2 `PositionInfo.priceResiduePNSQ16` field. The SC only persists
    // residue on `PositionIncreasedV2`, so we open the taker position with
    // a clean fill then *increase* it with a second averaged fill - the
    // combined average lands between PNS ticks and is stored as a non-zero
    // residue. With BTC perp's `price_decimals=1`, PNS tick is 0.1.
    //
    //   Open:     0.1 @ 100000                  -> entry 100000  (clean)
    //   Increase: 0.2 @ 100002                  -> combined avg
    //   = (0.1 * 100000 + 0.2 * 100002) / 0.3  = 100001.333...
    //   = PNS 1000013.333 -> stored 1000014, residue = floor(0.333 * 2^16).
    o(maker.id, 1, None, OpenShort, udec64!(100000), udec64!(0.1), None).await;
    o(taker.id, 2, None, OpenLong, udec64!(100000), udec64!(0.1), None).await;
    o(maker.id, 3, None, OpenShort, udec64!(100002), udec64!(0.2), None).await;
    o(taker.id, 4, None, OpenLong, udec64!(100002), udec64!(0.2), None).await;

    // Snapshot the chain - exercises V2 `getPerpetualInfoV2` / `getPositionV2`
    let (indexer, mut state) = testing::Indexer::new(&exchange).await;

    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();

        // V2 `PerpetualInfo.fundingSumScalingExp` was applied to the
        // `funding_sum_converter` (combines exp with price decimals).
        assert_eq!(
            perp.funding_sum_converter().decimals(),
            initial_funding_exp + perp.price_converter().decimals(),
        );

        // V2 `PositionInfo.priceResiduePNSQ16` preserves fractional entry
        // price beyond the 0.1 PNS tick. Without the residue the snapshot
        // would round to 100001.3 or 100001.4 (depending on direction);
        // with it the value sits between, ~100001.333.
        let taker_pos = snapshot
            .accounts()
            .get(&taker.id)
            .unwrap()
            .positions()
            .get(&btc_perp.id)
            .unwrap();
        assert_eq!(taker_pos.r#type(), state::PositionType::Long);
        assert_eq!(taker_pos.size(), udec64!(0.3));
        assert_eq!(
            taker_pos.entry_price(),
            udec64!(100001.33333282470703),
            "entry_price should reflect non-zero priceResiduePNSQ16",
        );
    }

    // Now exercise the same V2 fields via the live event stream.
    tokio::spawn(indexer.run(tokio::time::sleep));

    // Increase the taker's position with another averaged fill (0.1@100005
    // + 0.2@100007 = avg 100006.333). Combined with the existing 0.3@100001.333
    // the new entry is (0.3*100001.333 + 0.3*100006.333)/0.6 = 100003.833 -
    // also off-tick, so the `PositionIncreasedV2` event must carry a
    // non-zero `priceResiduePNSQ16` that the stream handler applies.
    o(maker.id, 10, None, OpenShort, udec64!(100005), udec64!(0.1), None).await;
    o(maker.id, 11, None, OpenShort, udec64!(100007), udec64!(0.2), None).await;
    o(taker.id, 12, None, OpenLong, udec64!(100007), udec64!(0.3), None).await;

    assert!(
        tokio::time::timeout(Duration::from_secs(5), state.wait_for(None, Some(12)))
            .await
            .unwrap()
    );

    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();

        // Funding sum scaling exp persists through event-stream updates.
        assert_eq!(
            perp.funding_sum_converter().decimals(),
            initial_funding_exp + perp.price_converter().decimals(),
        );

        // `PositionIncreasedV2.priceResiduePNSQ16` applied via event stream.
        let taker_pos = snapshot
            .accounts()
            .get(&taker.id)
            .unwrap()
            .positions()
            .get(&btc_perp.id)
            .unwrap();
        assert_eq!(taker_pos.size(), udec64!(0.6));
        assert_eq!(
            taker_pos.entry_price(),
            udec64!(100003.86666412353515),
            "entry_price should reflect averaged-fill priceResiduePNSQ16",
        );
    }
}
