use std::{num::NonZeroU16, pin::pin, time::Duration};

use alloy::{
    eips::BlockId,
    providers::ProviderBuilder,
    rpc::client::RpcClient,
    transports::layers::{RetryBackoffLayer, ThrottleLayer},
};
use fastnum::UD64;
use futures::StreamExt;
use perpl_sdk::{Chain, state::SnapshotBuilder, stream};

/// Tests order book state tracking at mainnet blocks 68746821–68746822 where
/// maker Close order gets implicitly removed due to capping by position size
#[tokio::test]
async fn test_maker_close_order_implicit_removal() {
    let chain = Chain::mainnet();
    let client = RpcClient::builder()
        .layer(ThrottleLayer::new(15))
        .layer(RetryBackoffLayer::new(10, 100, 200))
        .connect("https://rpc-mainnet.monadinfra.com")
        .await
        .unwrap();
    client.set_poll_interval(Duration::from_millis(100));
    let provider = ProviderBuilder::new().connect_client(client);

    let builder =
        SnapshotBuilder::new(&chain, provider.clone()).at_block(BlockId::number(68746821));
    let mut exchange = builder.build().await.unwrap();

    let stream = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut stream = pin!(stream);
    let block_events = stream.next().await.unwrap().unwrap();
    exchange.apply_events(&block_events).unwrap();

    assert!(
        exchange
            .perpetuals()
            .get(&10)
            .unwrap()
            .l3_book()
            .get_order(NonZeroU16::new(16).unwrap())
            .is_none()
    );
}

/// Tests all-positions snapshot creation at historical block with
/// updates applied on top of it. Exercises the V0 fallback path in
/// `SnapshotBuilder` since the mainnet contract at this block predates
/// the V2 getters.
#[tokio::test]
async fn test_all_positions_snapshot_and_updates() {
    let chain = Chain::mainnet();
    let client = RpcClient::builder()
        .layer(ThrottleLayer::new(15))
        .layer(RetryBackoffLayer::new(10, 100, 200))
        .connect("https://rpc-mainnet.monadinfra.com")
        .await
        .unwrap();
    client.set_poll_interval(Duration::from_millis(100));
    let provider = ProviderBuilder::new().connect_client(client);

    let snapshot_block = 68746821;
    let builder = SnapshotBuilder::new(&chain, provider.clone())
        .at_block(BlockId::number(snapshot_block))
        .with_all_positions();
    let mut exchange = builder.build().await.unwrap();

    // Snapshot must include every configured perpetual with V2-equivalent
    // metadata populated. Pre-V2 contracts return no `fundingSumScalingExp`,
    // so the fallback path defaults the funding scaling to the price scale.
    assert_eq!(exchange.perpetuals().len(), chain.perpetuals().len());
    for perp_id in chain.perpetuals() {
        let perp = exchange
            .perpetuals()
            .get(perp_id)
            .unwrap_or_else(|| panic!("perp {perp_id} missing from snapshot"));
        assert!(!perp.name().is_empty(), "perp {perp_id} has empty name");
        assert!(!perp.symbol().is_empty(), "perp {perp_id} has empty symbol");
        assert!(perp.price_converter().decimals() > 0, "perp {perp_id} has zero price decimals");
        // V0 fallback => fundingSumScalingExp defaults to 0, so funding sum
        // converter must collapse to the price decimals scale.
        assert_eq!(
            perp.funding_sum_converter().decimals(),
            perp.price_converter().decimals(),
            "perp {perp_id} funding_sum_converter scale mismatch with V0 fallback",
        );
    }

    // `with_all_positions()` must have populated at least one account with at
    // least one open position decoded via the V0 fallback path.
    assert!(!exchange.accounts().is_empty(), "no accounts loaded");
    let total_positions: usize = exchange
        .accounts()
        .values()
        .map(|a| a.positions().len())
        .sum();
    assert!(total_positions > 0, "no positions loaded");
    let any_position = exchange
        .accounts()
        .values()
        .flat_map(|a| a.positions().values())
        .next()
        .unwrap();
    assert!(any_position.size() > UD64::ZERO);
    assert!(any_position.entry_price() > UD64::ZERO);

    // Apply the next 1000 blocks of events on top of the snapshot to verify
    // the V0-derived state stays consistent under update. This window covers
    // two state-tracking edge cases where logs are returned by Monad RPC in
    // non-monotonic tx order:
    //   - block 68747066
    //   - block 68747089
    let stream_blocks: usize = 1000;
    let stream = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut stream = pin!(stream.take(stream_blocks));
    while let Some(block_events) = stream.next().await {
        exchange.apply_events(&block_events.unwrap()).unwrap();
    }
    assert_eq!(exchange.instant().block_number(), snapshot_block + stream_blocks as u64);
}
