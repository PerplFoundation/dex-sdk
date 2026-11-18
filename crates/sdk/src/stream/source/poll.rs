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
    primitives::{Address, B256, Bloom, BloomInput},
    providers::Provider,
    rpc::types::{Filter, Log},
    sol_types::SolEventInterface,
};

use crate::{
    Chain, abi::dex::Exchange::ExchangeEvents, error::ProviderError, stream::RawEvent, types,
};

/// One chain tip: the newest block in a commit state, by height, with its
/// hash and parent when the node returned a block (none ⇒ height 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tip {
    pub number: u64,
    pub hash: Option<B256>,
    pub parent: Option<B256>,
}

impl Tip {
    /// A tip of a known height whose block was not fetched.
    pub fn at(number: u64) -> Self { Self { number, hash: None, parent: None } }

    fn from_block(b: Option<alloy::rpc::types::Block>) -> Self {
        b.map(|b| Self {
            number: b.header.number,
            hash: Some(b.header.hash),
            parent: Some(b.header.parent_hash),
        })
        .unwrap_or_default()
    }

    /// The higher of the two tips; a stale lower tip only understates
    /// progress, so the higher one stands in for it.
    fn max(self, other: Self) -> Self { if other.number > self.number { other } else { self } }
}

/// The three chain tips, each the newest block in its commit state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tips {
    pub latest: Tip,
    pub safe: Tip,
    pub finalized: Tip,
}

impl Tips {
    /// Tips that never go backwards: a lower tip reported behind a higher
    /// one is stood in for by the higher one.
    pub fn new(latest: Tip, safe: Tip, finalized: Tip) -> Self {
        let safe = safe.max(finalized);
        let latest = latest.max(safe);
        Self { latest, safe, finalized }
    }

    /// The blocks the tips name by hash, each with the state it has reached,
    /// highest state first. The parent of `latest` is the block whose QC the
    /// latest proposal carries, hence Voted (that is what Voted means to the
    /// node).
    pub fn named(&self) -> impl Iterator<Item = (u64, B256, types::CommitState)> {
        use types::CommitState::*;
        [
            (self.finalized.number, self.finalized.hash, Finalized),
            (self.safe.number, self.safe.hash, Voted),
            (self.latest.number.wrapping_sub(1), self.latest.parent, Voted),
            (self.latest.number, self.latest.hash, Proposed),
        ]
        .into_iter()
        .filter_map(|(n, h, s)| Some((n, h?, s)))
    }

    /// Hash of the block at `height` if one of the tips names it.
    pub fn hash_at(&self, height: u64) -> Option<B256> {
        self.named()
            .find_map(|(n, h, _)| (n == height).then_some(h))
    }

    /// Commit state a block at `height` is known to have reached according to
    /// these tips. Conservative: a stale `Tips` only understates progress.
    pub fn state_of(&self, height: u64) -> types::CommitState {
        if height <= self.finalized.number {
            types::CommitState::Finalized
        } else if height <= self.safe.number {
            types::CommitState::Voted
        } else {
            types::CommitState::Proposed
        }
    }

    /// Height of the tip for the given commit state.
    pub fn at(&self, state: types::CommitState) -> u64 {
        match state.normalized() {
            types::CommitState::Proposed => self.latest.number,
            types::CommitState::Voted => self.safe.number,
            _ => self.finalized.number,
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

/// Whether a block with this logs bloom may hold logs of the exchange.
pub fn may_have_logs(bloom: &Bloom, exchange: Address) -> bool {
    bloom.contains_input(BloomInput::Raw(exchange.as_slice()))
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

/// Fetches the `finalized` tip and, when `speculative` names a state below
/// `Finalized`, the `latest` tip and (for `Voted`) the `safe` tip too, so
/// that polling can prefetch proposed blocks and emit them the moment the
/// `safe` tip names them. Tips not fetched are reported at the finalized
/// height, which only ever understates progress.
pub async fn tips<P: Provider>(
    provider: &P,
    speculative: Option<types::CommitState>,
) -> Result<Tips> {
    let finalized = provider.get_block(BlockId::finalized()).into_future();
    let latest = || provider.get_block(BlockId::latest()).into_future();
    match speculative.map(types::CommitState::normalized) {
        Some(types::CommitState::Proposed) => {
            let (finalized, latest) = futures::try_join!(finalized, latest())?;
            let (finalized, latest) = (Tip::from_block(finalized), Tip::from_block(latest));
            Ok(Tips::new(latest, finalized, finalized))
        },
        Some(types::CommitState::Voted) => {
            let safe = provider.get_block(BlockId::safe()).into_future();
            let (finalized, safe, latest) = futures::try_join!(finalized, safe, latest())?;
            Ok(Tips::new(
                Tip::from_block(latest),
                Tip::from_block(safe),
                Tip::from_block(finalized),
            ))
        },
        _ => {
            let finalized = Tip::from_block(finalized.await?);
            Ok(Tips::new(finalized, finalized, finalized))
        },
    }
}

/// Fetches the header of a block by number, with its logs bloom, `None`
/// when the node does not have it yet.
pub async fn header<P: Provider>(
    provider: &P,
    number: u64,
) -> Result<Option<(BlockHeader, Bloom)>> {
    let block = provider.get_block(BlockId::number(number)).await?;
    Ok(block.map(|b| (BlockHeader::from(&b.header), b.header.logs_bloom)))
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
/// given headers, each with whether its bloom may hold exchange logs.
///
/// A block without logs gets an empty event list if its bloom rules them
/// out; otherwise the response may just not cover it (a backend behind the
/// range's end floors `toBlock` to its head without an error), so the blocks
/// stop before it.
pub fn split_range(
    logs: &[Log],
    headers: impl IntoIterator<Item = (BlockHeader, bool)>,
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
        .take_while(|(header, may_have_logs)| {
            !may_have_logs || by_block.contains_key(&header.number)
        })
        .map(|(header, _)| {
            let logs: Vec<Log> = by_block
                .get(&header.number)
                .map(|ls| ls.iter().map(|l| (*l).clone()).collect())
                .unwrap_or_default();
            Ok(PolledBlock { header, events: decode_logs(&logs, header.hash)? })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use alloy::{
        primitives::{Address, LogData, U256},
        sol_types::SolEvent,
    };

    use super::*;
    use crate::abi::dex::Exchange::AccountCreated;

    fn header(number: u64) -> BlockHeader {
        BlockHeader {
            number,
            hash: B256::from(U256::from(number)),
            parent: B256::from(U256::from(number - 1)),
            timestamp: number,
        }
    }

    fn log(number: u64) -> Log {
        let data: LogData =
            AccountCreated { id: U256::from(number), account: Address::ZERO }.encode_log_data();
        Log {
            inner: alloy::primitives::Log { address: Address::ZERO, data },
            block_hash: Some(header(number).hash),
            block_number: Some(number),
            ..Default::default()
        }
    }

    /// A backend whose head lags the range floors `toBlock` without an
    /// error: the heights it did not cover are not known empty unless their
    /// bloom says so.
    #[test]
    fn split_range_stops_at_a_block_whose_logs_may_be_missing() {
        let logs = [log(100), log(101), log(102)];
        let headers = (100..=105).map(|n| (header(n), n != 103));
        let blocks = split_range(&logs, headers).unwrap();
        let got: Vec<_> = blocks
            .iter()
            .map(|b| (b.header.number, b.events.len()))
            .collect();
        assert_eq!(got, vec![(100, 1), (101, 1), (102, 1), (103, 0)]);
    }
}
