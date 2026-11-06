//! Polling primitives shared by [`super::EventSource`] and
//! [`crate::stream::raw`]: chain tips, hash-verified per-block fetches and
//! `eth_getLogs` ranges.
//!
//! Every fetch verifies that each log's `blockHash` matches the header it is
//! paired with. Monad serves proposed blocks from a buffer that replaces a
//! block in place on a fork, and its `eth_getLogs` does not verify the served
//! block against a `blockHash` filter, so a header and a log set fetched in two
//! requests can belong to different proposals at the same height.

use alloy::{
    eips::BlockId,
    primitives::B256,
    providers::Provider,
    rpc::types::{Filter, Log},
    sol_types::SolEventInterface,
};

use crate::{
    Chain, abi::dex::Exchange::ExchangeEvents, error::ProviderError, stream::RawEvent, types,
};

/// Heights of the three chain tips, each the newest block in its commit state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tips {
    pub latest: u64,
    pub safe: u64,
    pub finalized: u64,
}

impl Tips {
    /// Commit state a block at `height` is known to have reached according to
    /// these tips. Conservative: a stale `Tips` only understates progress.
    pub fn state_of(&self, height: u64) -> types::CommitState {
        if height <= self.finalized {
            types::CommitState::Finalized
        } else if height <= self.safe {
            types::CommitState::Voted
        } else {
            types::CommitState::Proposed
        }
    }

    /// Height of the tip for the given commit state.
    pub fn at(&self, state: types::CommitState) -> u64 {
        match state.normalized() {
            types::CommitState::Proposed => self.latest,
            types::CommitState::Voted => self.safe,
            _ => self.finalized,
        }
    }
}

/// Header essentials of a fetched block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    pub number: u64,
    pub hash: B256,
    pub parent: B256,
    pub timestamp: u64,
}

impl From<&alloy::rpc::types::Header> for BlockHeader {
    fn from(h: &alloy::rpc::types::Header) -> Self {
        Self { number: h.number, hash: h.hash, parent: h.parent_hash, timestamp: h.timestamp }
    }
}

/// A block fetched by polling: header plus its decoded exchange events, in
/// log-index order, verified to belong to the header's hash.
#[derive(Clone, Debug)]
pub struct PolledBlock {
    pub header: BlockHeader,
    pub events: Vec<RawEvent>,
}

type Result<T> =
    std::result::Result<T, ProviderError<crate::abi::errors::Exchange::ExchangeErrors>>;

/// Fetches the height of the `finalized` tip and, when `speculative` names a
/// state below `Finalized`, of that state's tip too. Tips not fetched are
/// reported at the finalized height, which only ever understates progress.
pub async fn tips<P: Provider>(
    provider: &P,
    speculative: Option<types::CommitState>,
) -> Result<Tips> {
    let number = |b: Option<alloy::rpc::types::Block>| b.map(|b| b.header.number).unwrap_or(0);
    let finalized = provider.get_block(BlockId::finalized());
    match speculative.map(types::CommitState::normalized) {
        Some(state @ (types::CommitState::Proposed | types::CommitState::Voted)) => {
            let (finalized, tip) = futures::try_join!(
                finalized.into_future(),
                provider
                    .get_block(BlockId::Number(state.block_tag()))
                    .into_future()
            )?;
            let finalized = number(finalized);
            let tip = number(tip).max(finalized);
            Ok(match state {
                types::CommitState::Proposed => Tips { latest: tip, safe: finalized, finalized },
                _ => Tips { latest: tip, safe: tip, finalized },
            })
        },
        _ => {
            let finalized = number(finalized.await?);
            Ok(Tips { latest: finalized, safe: finalized, finalized })
        },
    }
}

/// Fetches the header of a block by number, `None` when the node does not
/// have it yet.
pub async fn header<P: Provider>(provider: &P, number: u64) -> Result<Option<BlockHeader>> {
    let block = provider.get_block(BlockId::number(number)).await?;
    Ok(block.map(|b| BlockHeader::from(&b.header)))
}

/// Decodes exchange logs into events, in log-index order, checking that every
/// log belongs to the block with the given hash.
///
/// Monad RPC does not guarantee logs are returned in block-internal order
/// (eg block 68747089 from https://rpc-mainnet.monadinfra.com), hence the sort.
pub fn decode_logs(logs: &[Log], block_hash: B256) -> Result<Vec<RawEvent>> {
    let mut events = Vec::with_capacity(logs.len());
    for log in logs {
        if log.block_hash.is_some_and(|h| h != block_hash) {
            return Err(ProviderError::InvalidRequest(format!(
                "log of block {} served for block {}",
                log.block_hash.unwrap_or_default(),
                block_hash
            )));
        }
        events.push(RawEvent::new(
            log.transaction_hash.unwrap_or_default(),
            log.transaction_index.unwrap_or_default(),
            log.log_index.unwrap_or_default(),
            ExchangeEvents::decode_log(&log.inner)
                .map_err(ProviderError::from)?
                .data,
        ));
    }
    events.sort_by_key(|e| e.log_index());
    Ok(events)
}

/// Fetches one block's header and exchange logs, `None` when the node does not
/// have the block yet. An `InvalidRequest` error means the two requests were
/// served from different proposals and the fetch should be retried.
pub async fn block<P: Provider>(
    provider: &P,
    chain: &Chain,
    number: u64,
) -> Result<Option<PolledBlock>> {
    let filter = Filter::new()
        .address(chain.exchange())
        .from_block(number)
        .to_block(number);
    // Anvil node, and maybe some RPC providers, produce empty response instead of
    // error in case the block in the filter does not exist yet, hence the explicit
    // header fetch.
    let (header, logs) = futures::try_join!(
        provider.get_block(BlockId::number(number)).into_future(),
        provider.get_logs(&filter)
    )?;
    let Some(header) = header else {
        return Ok(None);
    };
    let header = BlockHeader::from(&header.header);
    let events = decode_logs(&logs, header.hash)?;
    Ok(Some(PolledBlock { header, events }))
}

/// Result of a range fetch.
pub enum Range {
    /// Logs of `[from, to]`, as served.
    Logs(Vec<Log>),
    /// The node refused the span; retry with at most this many blocks.
    TooLarge,
}

/// Fetches the exchange logs of the inclusive block range in one request.
pub async fn range<P: Provider>(provider: &P, chain: &Chain, from: u64, to: u64) -> Result<Range> {
    let filter = Filter::new()
        .address(chain.exchange())
        .from_block(from)
        .to_block(to);
    match provider.get_logs(&filter).await {
        Ok(logs) => Ok(Range::Logs(logs)),
        Err(err) if is_range_too_large(&err) => Ok(Range::TooLarge),
        Err(err) => Err(err.into()),
    }
}

fn is_range_too_large<E>(err: &alloy::transports::RpcError<E>) -> bool {
    err.as_error_resp().is_some_and(|resp| {
        let msg = resp.message.to_ascii_lowercase();
        msg.contains("range") && (msg.contains("large") || msg.contains("limit"))
    })
}

/// Splits range logs per block, in log-index order, verified against the
/// given headers. Blocks without logs get an empty event list.
pub fn split_range(
    logs: &[Log],
    headers: impl IntoIterator<Item = BlockHeader>,
) -> Result<Vec<PolledBlock>> {
    let mut by_block: std::collections::BTreeMap<u64, Vec<&Log>> = Default::default();
    for log in logs {
        by_block
            .entry(log.block_number.unwrap_or_default())
            .or_default()
            .push(log);
    }
    headers
        .into_iter()
        .map(|header| {
            let logs: Vec<Log> = by_block
                .get(&header.number)
                .map(|ls| ls.iter().map(|l| (*l).clone()).collect())
                .unwrap_or_default();
            Ok(PolledBlock { header, events: decode_logs(&logs, header.hash)? })
        })
        .collect()
}
