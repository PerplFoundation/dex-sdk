use std::{pin::pin, time::Duration};

use alloy::{
    eips::BlockId,
    providers::{Provider, ProviderBuilder},
    rpc::client::RpcClient,
    transports::layers::{RetryBackoffLayer, ThrottleLayer},
};
use fastnum::UD64;
use futures::StreamExt;
use perpl_sdk::{
    Chain,
    state::{self, ContractVersion, SnapshotBuilder},
    stream,
};

/// Blocks of events applied on top of the snapshot.
const STREAM_BLOCKS: usize = 100;

/// Tests all-positions snapshot creation with updates applied on top of it,
/// against the contract **currently deployed on mainnet**.
///
/// Mainnet runs v1.1.7.5, so this exercises the fully-featured path rather than
/// a degraded one: the contract answers `getContractVersion()`, which settles
/// the feature set outright and means no selector probing happens at all; the
/// perpetual set is read from the existence bitmap; fees come from a keyed
/// 8-tier schedule; and the event stream carries the V2 order/fill events.
///
/// It is therefore no longer the SDK's compatibility test for an old contract
/// version, which is what it was while mainnet predated v1.1.7.4. The paths it
/// used to cover - probing the feature set, discovering perpetuals by probing
/// the ID space, flat per-contract fee getters, V1 events - are covered by
/// `sc_v1173.rs` and `sc_upgrade.rs` against locally deployed contracts.
/// Keeping that coverage here by pinning a pre-upgrade block is not an option:
/// the snapshot needs a thousand-odd position reads per perpetual, which a
/// public RPC will not serve against archive state.
///
/// That same recent-window requirement costs the coverage of two pinned blocks
/// (68747066 and 68747089) where Monad RPC returned logs out of transaction
/// order - the handling of that is `stream::raw` sorting on log index.
#[tokio::test]
async fn test_all_positions_snapshot_and_updates() {
    // Empty perpetual list: the set of listed contracts is discovered on-chain
    let chain = Chain::mainnet();
    let client = RpcClient::builder()
        .layer(ThrottleLayer::new(15))
        .layer(RetryBackoffLayer::new(10, 100, 200))
        .connect("https://rpc-mainnet.monadinfra.com")
        .await
        .unwrap();
    client.set_poll_interval(Duration::from_millis(100));
    let provider = ProviderBuilder::new().connect_client(client);

    // Snapshot far enough back that the whole streamed window is already voted
    // on, so the test never waits on block production
    let safe_block = provider
        .get_block(BlockId::safe())
        .await
        .unwrap()
        .expect("safe block")
        .header
        .number;
    let snapshot_block = safe_block - STREAM_BLOCKS as u64;

    let builder = SnapshotBuilder::new(&chain, provider.clone())
        .at_block(BlockId::number(snapshot_block))
        .with_all_positions();
    let mut exchange = builder.build().await.unwrap();

    // v1.1.7.4+ reports its own version, so the feature set is read rather than
    // inferred and every flag below follows from the version alone. Nothing
    // further down distinguishes v1.1.7.5 from v1.1.7.4 - the ppm fee unit is
    // the only thing that release changed here, and it is not asserted - so
    // this equality is what pins the deployment the rest of the test describes.
    let features = exchange.features();
    assert_eq!(
        exchange.contract_version(),
        Some(ContractVersion::new(1, 7, 5)),
        "mainnet moved off v1.1.7.5 - recheck which paths below are still live",
    );
    assert!(features.keyed_fee_schedules());
    assert!(features.builder_attribution());
    assert!(features.perpetual_discovery());

    // Perpetuals come from the existence bitmap, less the ones the chain
    // excludes. Filtering the bitmap's result is enough here - it is one call
    // whatever it returns; it is the probing fallback that has to skip the
    // excluded ids up front, since probing them is itself what they cost
    assert!(!exchange.perpetuals().is_empty(), "no perpetuals discovered");
    for excluded in chain.excluded_perpetuals() {
        assert!(!exchange.perpetuals().contains_key(excluded), "perp {excluded} was excluded");
    }
    for (perp_id, perp) in exchange.perpetuals() {
        assert!(!perp.name().is_empty(), "perp {perp_id} has empty name");
        assert!(!perp.symbol().is_empty(), "perp {perp_id} has empty symbol");
        assert!(perp.price_converter().decimals() > 0, "perp {perp_id} has zero price decimals");
        // The funding sum converter combines the per-perp scaling exponent with
        // the price scale, so it can only be at or above the latter
        assert!(
            perp.funding_sum_converter().decimals() >= perp.price_converter().decimals(),
            "perp {perp_id} funding_sum_converter scale below price scale",
        );
        // A real keyed schedule, not the flat per-contract pair standing in for
        // one: every mainnet perpetual still resolves to the exchange-wide
        // default rather than a custom key, and its eight tiers genuinely
        // differ - which is what the tier comparisons below, and not the
        // `base_taker_fee` check, are there to catch
        let schedule = perp.fee_schedule();
        assert_eq!(schedule.key(), state::FeeScheduleKey::Default);
        assert!(schedule.base_taker_fee() > UD64::ZERO, "perp {perp_id} has zero taker fee");
        assert!(
            schedule.taker_fee(7) < schedule.taker_fee(1),
            "perp {perp_id} has no taker tiers to differ by",
        );
        assert!(
            schedule.maker_fee(7) < schedule.maker_fee(1),
            "perp {perp_id} has no maker tiers to differ by",
        );
    }

    // `with_all_positions()` must have populated at least one account with at
    // least one open position. Such accounts are not snapshotted individually,
    // so they carry no fee tier.
    assert!(!exchange.accounts().is_empty(), "no accounts loaded");
    let total_positions: usize = exchange
        .accounts()
        .values()
        .map(|a| a.positions().len())
        .sum();
    assert!(total_positions > 0, "no positions loaded");
    assert!(exchange.accounts().values().all(|a| a.fee_tier().is_none()));
    let any_position = exchange
        .accounts()
        .values()
        .flat_map(|a| a.positions().values())
        .next()
        .unwrap();
    assert!(any_position.size() > UD64::ZERO);
    assert!(any_position.entry_price() > UD64::ZERO);

    // Apply the following blocks of events on top of the snapshot to verify the
    // state stays consistent under update against the live contract's event set.
    let stream = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut stream = pin!(stream.take(STREAM_BLOCKS));
    while let Some(block_events) = stream.next().await {
        exchange.apply_events(&block_events.unwrap()).unwrap();
    }
    assert_eq!(exchange.instant().block_number(), snapshot_block + STREAM_BLOCKS as u64);
}
