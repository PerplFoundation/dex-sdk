use std::{num::NonZeroU16, pin::pin, time::Duration};

use alloy::{
    eips::BlockId,
    providers::ProviderBuilder,
    rpc::client::RpcClient,
    transports::layers::{RetryBackoffLayer, ThrottleLayer},
};
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
