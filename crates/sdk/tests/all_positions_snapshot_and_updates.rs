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
/// That deployment predates v1.1.7.4, which makes this the SDK's compatibility
/// test for an old contract version: with no version getter to read, the
/// feature set is resolved by selector probing; the perpetual set is discovered
/// by probing the ID space rather than read from the existence bitmap; fees
/// come from the per-contract getters instead of a keyed schedule; and the
/// event stream carries the V1 order/fill events. Everything the upgrade
/// introduced must degrade rather than fail.
///
/// The window is recent rather than pinned: the snapshot needs a thousand-odd
/// position reads per perpetual, which a public RPC will not serve against
/// archive state. That costs the coverage of two pinned blocks (68747066 and
/// 68747089) where Monad RPC returned logs out of transaction order - the
/// handling of that is `stream::raw` sorting on log index.
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

    // Pre-v1.1.7.4: no version getter, so none of the features that release
    // introduced are assumed present
    let features = exchange.features();
    assert_eq!(
        exchange.contract_version(),
        Some(ContractVersion::new(1, 7, 4)),
        "mainnet is not upgraded yet"
    );
    assert!(features.keyed_fee_schedules());
    assert!(features.builder_attribution());
    assert!(features.perpetual_discovery());

    // Perpetuals were still found, by probing the ID space, less the ones the
    // chain excludes - which the probe must skip rather than filter afterwards,
    // since probing them is itself part of what they cost
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
        // Without keyed schedules the per-contract fee getters stand in for one:
        // flat across every tier, under the default key
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
    // state stays consistent under update against the old contract's event set.
    let stream = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut stream = pin!(stream.take(STREAM_BLOCKS));
    while let Some(block_events) = stream.next().await {
        exchange.apply_events(&block_events.unwrap()).unwrap();
    }
    assert_eq!(exchange.instant().block_number(), snapshot_block + STREAM_BLOCKS as u64);
}
