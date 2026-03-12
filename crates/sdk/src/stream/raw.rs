use std::time::Duration;

use alloy::{eips::BlockId, providers::Provider, rpc::types::Filter, sol_types::SolEventInterface};
use futures::{Stream, stream};

use crate::{
    Chain,
    abi::dex::Exchange::ExchangeEvents,
    error::{DexError, ProviderError},
    types,
};

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
/// See [`crate::abi::dex::Exchange::ExchangeEvents`] for the list of possible
/// events and corresponding details.
///
/// # Safety note
///
/// The returned stream is not cancellation-safe and should not be used within
/// `select!`.
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
    stream::unfold((provider, from.block_number()), move |(provider, mut block_num)| async move {
        let filter = Filter::new()
            .address(chain.exchange())
            .from_block(block_num)
            .to_block(block_num);
        loop {
            // Anvil node, and maybe some RPC providers, produce empty response instead of
            // error in case the block in the filter does not exist yet.
            // Checking the block presence explicitly, and also checking requested block
            // number against `safe` block tag as Monad RPC assumes `latest` ==
            // `Proposed` since v0.13.0 and `Proposed` is not safe enough to
            // preserve state consistency.
            let result = futures::try_join!(
                provider.get_block(BlockId::safe()).into_future(),
                provider.get_block(BlockId::number(block_num)).into_future(),
                provider.get_logs(&filter)
            )
            .map_err(ProviderError::from)
            .and_then(|(safe_block, block, logs)| {
                if safe_block.is_none_or(|sb| sb.header.number < block_num) {
                    return Err(ProviderError::InvalidRequest(
                        "block is not available yet".to_string(),
                    ));
                }
                let block_header = block
                    .ok_or(ProviderError::InvalidRequest("block is not available yet".to_string()))?
                    .header;
                let mut events = Vec::with_capacity(logs.len());
                for log in &logs {
                    events.push(RawEvent::new(
                        log.transaction_hash.unwrap_or_default(),
                        log.transaction_index.unwrap_or_default(),
                        log.log_index.unwrap_or_default(),
                        ExchangeEvents::decode_log(&log.inner)
                            .map_err(ProviderError::from)?
                            .data,
                    ));
                }
                Ok(RawBlockEvents::new(
                    types::StateInstant::new(block_num, block_header.timestamp),
                    events,
                ))
            });
            if result.is_ok() {
                block_num += 1;
                return Some((result.map_err(DexError::Provider), (provider, block_num)));
            }
            if matches!(result, Err(ProviderError::InvalidRequest(_))) {
                // Block is not available yet
                sleep(provider.client().poll_interval()).await;
                continue;
            }
            return Some((result.map_err(DexError::Provider), (provider, block_num)));
        }
    })
}

#[cfg(test)]
mod tests {
    use alloy::{
        providers::ProviderBuilder, rpc::client::RpcClient, transports::layers::RetryBackoffLayer,
    };
    use futures::StreamExt;

    use super::*;
    use crate::Chain;

    #[tokio::test]
    #[ignore = "temporary ignored until updated smart contract is deployed"]
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
        let stream =
            raw(&testnet, provider, types::StateInstant::new(block_num, 0), tokio::time::sleep);
        let block_results = stream.take(10).collect::<Vec<_>>().await;

        for b in &block_results {
            assert_eq!(b.as_ref().unwrap().instant().block_number(), block_num);
            block_num += 1;
        }
    }
}
