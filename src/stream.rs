use std::time::Duration;

use alloy::{providers::Provider, rpc::types::Filter, sol_types::SolEventInterface};
use futures::{Stream, stream};

use crate::{Chain, abi::dex::Exchange::ExchangeEvents, error::DexError, types};

pub type RawEvent = types::EventContext<ExchangeEvents>;
pub type RawBlockEvents = types::BlockEvents<RawEvent>;

/// Returns stream of raw events emitted by the DEX smart contract,
/// batched per block, starting from the specified block.
///
/// Polls logs via the given [`Provider`] to produce strictly continuous
/// event sequence, with [`Provider`]-configured interval.
///
/// It is recommended to setup provider with
/// [`alloy::transports::layers::FallbackLayer`]
/// and/or [`alloy::transports::layers::RetryBackoffLayer`].
///
/// See [`crate::abi::dex::Exchange::ExchangeEvents`] for the list of possible events and corresponding details.
///
pub fn raw<P, S, SFut>(
    chain: &Chain,
    provider: P,
    from: types::StateInstant,
    sleep: S,
) -> impl Stream<Item = Result<RawBlockEvents, DexError>>
where
    P: Provider,
    S: Fn(Duration) -> SFut + Copy,
    SFut: Future<Output = ()>,
{
    stream::unfold(
        (provider, from.block_number()),
        move |(provider, mut block_num)| async move {
            let filter = Filter::new()
                .address(chain.exchange())
                .from_block(block_num)
                .to_block(block_num);
            loop {
                // Anvil node, and maybe some RPC providers, produce empty response instead of
                // error in case the block in the filter does not exist yet,
                // so adding aditional check against the tip of the chain
                let result =
                    futures::try_join!(provider.get_block_number(), provider.get_logs(&filter))
                        .map_err(DexError::from)
                        .and_then(|(head_block_num, logs)| {
                            if head_block_num < block_num {
                                return Err(DexError::InvalidRequest(
                                    "block is not available yet".to_string(),
                                ));
                            }
                            let mut events = Vec::with_capacity(logs.len());
                            let block_ts = logs.first().and_then(|l| l.block_timestamp);
                            for log in &logs {
                                events.push(RawEvent::new(
                                    log.transaction_hash.unwrap_or_default(),
                                    log.transaction_index.unwrap_or_default(),
                                    log.log_index.unwrap_or_default(),
                                    ExchangeEvents::decode_log(&log.inner)
                                        .map_err(DexError::from)?
                                        .data,
                                ));
                            }
                            Ok(RawBlockEvents::new(
                                types::StateInstant::new(block_num, block_ts.unwrap_or_default()),
                                events,
                            ))
                        });
                if result.is_ok() {
                    block_num += 1;
                    return Some((result, (provider, block_num)));
                }
                if matches!(result, Err(DexError::InvalidRequest(_))) {
                    // Block is not available yet
                    sleep(provider.client().poll_interval()).await;
                    continue;
                }
                return Some((result, (provider, block_num)));
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use alloy::{
        primitives::{U256, b256},
        providers::ProviderBuilder,
        rpc::client::RpcClient,
        transports::layers::RetryBackoffLayer,
    };
    use futures::StreamExt;

    use super::*;
    use crate::{Chain, abi::dex::Exchange::ExchangeEvents};

    #[tokio::test]
    #[ignore = "smart contract is not deployed yet"]
    async fn test_stream_historical_blocks() {
        let provider = ProviderBuilder::new()
            .connect("https://testnet-rpc.monad.xyz")
            .await
            .unwrap();

        let testnet = Chain::testnet();
        let from_block = 41753780;
        let stream = raw(
            &testnet,
            provider,
            types::StateInstant::new(from_block, 0),
            tokio::time::sleep,
        );
        let block_results = stream.take(100).collect::<Vec<_>>().await;

        let block = block_results[0].as_ref().unwrap();
        assert_eq!(block.instant().block_number(), 41753780);
        assert_eq!(block.instant().block_timestamp(), 1759844205);
        assert_eq!(block.events().len(), 3);
        assert!(
            matches!(block.events()[0], RawEvent { tx_hash, tx_index: 5, log_index: 14, event: ExchangeEvents::OrderRequest(ref r)} if tx_hash == b256!("0x47de82c4aa40baa30cabac4a74568488a8c74ded85a4e905f1ceaad4f29945e3") && r.orderDescId == U256::from(1759844204673u64))
        );

        let block = block_results[2].as_ref().unwrap();
        assert_eq!(block.instant().block_number(), 41753782);
        assert_eq!(block.instant().block_timestamp(), 1759844206);
        assert_eq!(block.events().len(), 7);
        assert!(
            matches!(block.events()[0], RawEvent { tx_hash, tx_index: 2, log_index: 3, event: ExchangeEvents::LinkPriceUpdated(ref r)} if tx_hash == b256!("0xe2f90e72fd2c741ed02cfd7153e40d0d2d15472a44f5e9c30d3c9d189f02bcf6") && r.perpId == U256::from(64) && r.oraclePricePNS == U256::from(34552) && r.timestamp == U256::from(1759844205))
        );

        let mut block_num = from_block;
        for b in &block_results {
            if b.is_ok() {
                assert_eq!(b.as_ref().unwrap().instant().block_number(), block_num);
                block_num += 1;
            }
        }
    }

    #[tokio::test]
    #[ignore = "smart contract is not deployed yet"]
    async fn test_stream_recent_blocks() {
        let client = RpcClient::builder()
            .layer(RetryBackoffLayer::new(10, 100, 200))
            .connect("https://testnet-rpc.monad.xyz")
            .await
            .unwrap();
        client.set_poll_interval(Duration::from_millis(100));
        let provider = ProviderBuilder::new().connect_client(client);

        let testnet = Chain::testnet();
        let mut block_num = provider.get_block_number().await.unwrap() + 1;
        let stream = raw(
            &testnet,
            provider,
            types::StateInstant::new(block_num, 0),
            tokio::time::sleep,
        );
        let block_results = stream.take(10).collect::<Vec<_>>().await;

        for b in &block_results {
            assert_eq!(b.as_ref().unwrap().instant().block_number(), block_num);
            block_num += 1;
        }
    }
}
