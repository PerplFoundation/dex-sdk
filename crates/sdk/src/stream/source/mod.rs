//! Commit-state-aware exchange event source, see [`EventSource`].

mod adapters;
pub(crate) mod poll;
mod sequencer;
#[cfg(feature = "ws")]
mod ws;

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

pub use adapters::*;
use alloy::providers::Provider;
use futures::{
    Stream, StreamExt,
    stream::{self, BoxStream, SelectAll},
};
pub use poll::Tips;
pub use sequencer::Cursors;
use sequencer::{Action, Input, Output, Poll, PollRequest, Sequencer};

use super::RawBlockEvents;
use crate::{Chain, error::DexError, types};

/// Default `eth_getLogs` span: what the public Monad RPC endpoints allow
/// (`rpc.monad.xyz`, `rpc-mainnet.monadinfra.com`); a self-hosted node allows
/// 1000 by default. Clamped down automatically when the node refuses a span.
pub const DEFAULT_POLL_CHUNK: u64 = 100;
/// Default number of concurrent header fetches during gap fill.
pub const DEFAULT_POLL_CONCURRENCY: usize = 8;
/// Default period of the unconditional finalized-tip check.
pub const DEFAULT_TIP_INTERVAL: Duration = Duration::from_millis(500);
/// Default time without a header on a subscription before the tips are
/// re-checked; six such periods in a row rebuild the connection.
pub const DEFAULT_WATCHDOG: Duration = Duration::from_millis(500);
/// Default broadcast channel size per subscription (alloy's own default is
/// 16, which loses commit states under a burst).
pub const DEFAULT_CHANNEL_SIZE: usize = 4096;
const STALE_PERIODS: u32 = 6;

/// When a block delivered by a subscription is considered complete, i.e. all
/// of its exchange logs have arrived.
///
/// The node writes, per block and commit state, the `monadNewHeads` header
/// first, then every `monadLogs` log in log-index order, then the standard
/// `logs` of the same block (Proposed only), and never interleaves two blocks
/// on one connection. The policies differ in which of those facts they rely
/// on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealPolicy {
    /// Exact. A Proposed block is complete when the first standard `logs`
    /// frame for it arrives on the same connection, when a later header
    /// arrives, or immediately when the header's logs bloom excludes the
    /// exchange. Costs a second, Proposed-only `logs` subscription (about a
    /// quarter more frames) and adds a few milliseconds (p50 4-7 ms on the
    /// public endpoints). The default.
    Marker,
    /// Exact, no extra subscription. A block is complete when the next header
    /// of any block or state arrives on the connection, about one block
    /// interval later (p50 ~220 ms), which is roughly when the Voted update
    /// would arrive anyway.
    NextHeader,
    /// Heuristic. A block is complete when no log arrived for `quiet` after
    /// its header; a late log re-emits the block as superseding. Measured on
    /// the public endpoints: `quiet` = 10 ms gave zero or one re-emit per 200
    /// blocks, but a loaded node gave up to a quarter. Opt-in only.
    Quiescent { quiet: Duration },
}

/// Builder of a continuous, commit-state-tagged stream of exchange events.
///
/// The stream delivers every block from `from` onwards, each block as one
/// [`RawBlockEvents`] batch tagged with its [`types::CommitState`], with these
/// guarantees (`min` being [`Self::min_commit_state`]):
///
/// 1. **Finalized layer.** Exactly one batch tagged `Finalized` per height,
///    strictly sequential, never a gap, never a duplicate, never superseded.
/// 2. **Speculative layer** (only when `min < Finalized`). At most one batch
///    per `(height, block hash)`, tagged with the first state observed at or
///    above `min` and below `Finalized`. When the first chance to deliver a
///    height is already `Finalized` (gap fill after a reconnect, late start,
///    `Voted` skipped by the node), there is no speculative batch for it.
/// 3. **Speculative batches are sequential modulo rewinds.** A batch with
///    [`RawBlockEvents::supersedes`] set at height `h` invalidates every
///    earlier speculative batch at height `h` or above; the sequence resumes at
///    `h + 1`. A `Finalized` batch whose hash differs from the speculative one
///    emitted at its height is marked the same way.
/// 4. Speculative batches for a height precede its `Finalized` batch.
/// 5. `Err` items are non-recoverable and end the stream: the polling provider
///    failed after its own retry and fallback layers gave up, or a log could
///    not be decoded. Everything transient is retried internally, and
///    subscription loss degrades to polling, visible through
///    [`SourceHandle::status`].
///
/// # Sources
///
/// * **One polling provider**, exactly as [`super::raw`] takes it. Every
///   `eth_getLogs`, header and tip request goes through it. To multiplex or
///   fail over between HTTP endpoints, layer them with
///   [`alloy::transports::layers::FallbackLayer`] before passing the provider;
///   retries belong in [`alloy::transports::layers::RetryBackoffLayer`] the
///   same way.
/// * **Zero or more websocket URLs** ([`Self::with_ws_url`], feature `ws`),
///   each its own connection subscribed to `monadNewHeads`, `monadLogs` and,
///   under [`SealPolicy::Marker`], `logs`. The SDK owns the connections,
///   reconnects them with its own backoff and never sends requests on them. The
///   earliest delivery per block and commit state wins; the rest is
///   deduplicated.
/// * Without a websocket URL the source polls only, at [`Self::tip_interval`]:
///   a straight replacement for [`super::raw`] that also catches up in ranges
///   rather than block by block.
///
/// Subscriptions are only ever a latency optimisation: finality progress is
/// driven by periodic `finalized` tip polls, so a websocket that lags, lies,
/// or dies silently can delay the stream but never corrupt it.
///
/// # Safety note
///
/// The returned stream is not cancellation-safe and should not be used
/// within `select!`.
///
/// # Example
///
/// ```ignore
/// use std::pin::pin;
///
/// use futures::StreamExt;
/// use perpl_sdk::{Chain, state::SnapshotBuilder, stream, types::CommitState};
///
/// let chain = Chain::mainnet();
/// let mut exchange = SnapshotBuilder::new(&chain, provider.clone())
///     .at_commit_state(CommitState::Finalized)
///     .build()
///     .await?;
/// let (events, _handle) = stream::EventSource::new(&chain, provider, exchange.instant().next())
///     .min_commit_state(CommitState::Proposed)
///     .with_ws_url("wss://rpc.monad.xyz")
///     .build(tokio::time::sleep);
/// let mut events = pin!(events);
/// while let Some(block) = events.next().await {
///     let block = block?;
///     // Proposed/Voted batches update a speculative overlay, Finalized ones
///     // commit; every batch yields state events either way.
///     let state_events = exchange.apply_events_at(&block, CommitState::Finalized)?;
///     println!("{} {:?}: {} events", block.instant(), block.commit_state(), block.events().len());
/// }
/// ```
pub struct EventSource<P> {
    chain: Chain,
    provider: P,
    from: types::StateInstant,
    min: types::CommitState,
    seal: SealPolicy,
    poll_chunk: u64,
    poll_concurrency: usize,
    watchdog: Duration,
    tip_interval: Duration,
    channel_size: usize,
    ws_urls: Vec<String>,
}

impl<P> EventSource<P>
where
    P: Provider + Clone + 'static,
{
    /// Event source over the given polling provider, delivering every block
    /// from `from` onwards.
    pub fn new(chain: &Chain, provider: P, from: types::StateInstant) -> Self {
        Self {
            chain: chain.clone(),
            provider,
            from,
            min: types::CommitState::Voted,
            seal: SealPolicy::Marker,
            poll_chunk: DEFAULT_POLL_CHUNK,
            poll_concurrency: DEFAULT_POLL_CONCURRENCY,
            watchdog: DEFAULT_WATCHDOG,
            tip_interval: DEFAULT_TIP_INTERVAL,
            channel_size: DEFAULT_CHANNEL_SIZE,
            ws_urls: vec![],
        }
    }

    /// Lowest commit state to deliver. Default `Voted`, which is what
    /// [`super::raw`] delivers (it gates on the `safe` tag).
    ///
    /// `Proposed` delivers a block as soon as the node has executed it, about
    /// 300 ms (one block interval) before `Voted` and 600 ms before
    /// `Finalized`, at the price of the occasional superseding batch.
    pub fn min_commit_state(mut self, state: types::CommitState) -> Self {
        self.min = state.normalized();
        self
    }

    /// Websocket endpoint to subscribe to, e.g. `wss://rpc.monad.xyz`.
    /// Repeatable: every URL gets its own connection and the earliest
    /// delivery per block and commit state wins.
    ///
    /// Requires a tokio runtime, which alloy's pubsub service runs on.
    #[cfg(feature = "ws")]
    pub fn with_ws_url(mut self, url: impl Into<String>) -> Self {
        self.ws_urls.push(url.into());
        self
    }

    /// When a subscribed block is considered complete. Default
    /// [`SealPolicy::Marker`].
    pub fn seal_policy(mut self, policy: SealPolicy) -> Self {
        self.seal = policy;
        self
    }

    /// `eth_getLogs` span per gap-fill request. Default
    /// [`DEFAULT_POLL_CHUNK`]; clamped down automatically when the node
    /// refuses a span.
    pub fn poll_chunk(mut self, blocks: u64) -> Self {
        self.poll_chunk = blocks.max(1);
        self
    }

    /// Concurrent header fetches during gap fill, and the number of
    /// speculative heights polled at once. Default
    /// [`DEFAULT_POLL_CONCURRENCY`].
    pub fn poll_concurrency(mut self, n: usize) -> Self {
        self.poll_concurrency = n.max(1);
        self
    }

    /// Time without a header on a subscription before the tips are
    /// re-checked; six such periods in a row rebuild the connection. Default
    /// [`DEFAULT_WATCHDOG`].
    pub fn watchdog(mut self, period: Duration) -> Self {
        self.watchdog = period;
        self
    }

    /// Period of the unconditional `finalized` tip check on the polling
    /// provider, which drives gap detection and, without subscriptions, the
    /// stream itself. Default [`DEFAULT_TIP_INTERVAL`].
    pub fn tip_interval(mut self, period: Duration) -> Self {
        self.tip_interval = period;
        self
    }

    /// Broadcast channel size per subscription. Default
    /// [`DEFAULT_CHANNEL_SIZE`].
    pub fn channel_size(mut self, size: usize) -> Self {
        self.channel_size = size.max(1);
        self
    }

    /// Builds the stream and a handle to observe it. `sleep` is the
    /// runtime's timer, e.g. `tokio::time::sleep`.
    pub fn build<S, SFut>(
        self,
        sleep: S,
    ) -> (impl Stream<Item = Result<RawBlockEvents, DexError>> + Send + 'static, SourceHandle)
    where
        S: Fn(Duration) -> SFut + Copy + Send + Sync + 'static,
        SFut: Future<Output = ()> + Send + 'static,
    {
        let opts = sequencer::Options {
            min: self.min,
            seal: self.seal,
            poll_chunk: self.poll_chunk,
            poll_concurrency: self.poll_concurrency,
            tip_interval: self.tip_interval,
            watchdog: self.watchdog,
            stale_periods: STALE_PERIODS,
            sources: self.ws_urls.len(),
        };
        let status = Arc::new(Mutex::new(SourceStatus {
            sources: self
                .ws_urls
                .iter()
                .map(|url| WsStatus { url: url.clone(), ..Default::default() })
                .collect(),
            cursors: Cursors::default(),
        }));
        let handle = SourceHandle { status: status.clone() };
        let mut sequencer = Sequencer::new(self.from.block_number(), opts);
        #[allow(unused_mut)]
        let mut merged: SelectAll<BoxStream<'static, Input>> = SelectAll::new();
        #[allow(unused_mut)]
        let mut kicks = Vec::new();
        #[cfg(feature = "ws")]
        for (src, url) in self.ws_urls.iter().enumerate() {
            let (kick_tx, kick_rx) = futures::channel::mpsc::unbounded();
            kicks.push(kick_tx);
            merged.push(ws::source(
                src,
                url.clone(),
                self.chain.clone(),
                ws::Config {
                    marker: matches!(self.seal, SealPolicy::Marker),
                    channel_size: self.channel_size,
                },
                sleep,
                kick_rx,
                status.clone(),
            ));
        }
        let mut driver = Driver {
            chain: self.chain,
            provider: self.provider,
            sleep,
            concurrency: self.poll_concurrency,
            // With subscriptions only finality needs polling; without, the
            // speculative layer polls up to the tip of the minimum state.
            speculative_tip: (self.ws_urls.is_empty() && self.min < types::CommitState::Finalized)
                .then_some(self.min),
            merged,
            kicks,
            status,
            outputs: VecDeque::new(),
            done: false,
        };
        for action in sequencer.start() {
            driver.perform(action);
        }
        let stream =
            stream::unfold((driver, sequencer), |(mut driver, mut sequencer)| async move {
                driver
                    .next(&mut sequencer)
                    .await
                    .map(|item| (item, (driver, sequencer)))
            });
        (stream, handle)
    }
}

/// Observer of a running [`EventSource`].
#[derive(Clone, Debug)]
pub struct SourceHandle {
    status: Arc<Mutex<SourceStatus>>,
}

impl SourceHandle {
    /// Current state of every subscription and of the sequencing cursors.
    pub fn status(&self) -> SourceStatus { self.status.lock().unwrap().clone() }
}

/// Snapshot of an [`EventSource`]'s internals.
#[derive(Clone, Debug, Default)]
pub struct SourceStatus {
    /// One entry per websocket URL, in the order given.
    pub sources: Vec<WsStatus>,
    pub cursors: Cursors,
}

/// State of one websocket connection.
#[derive(Clone, Debug, Default)]
pub struct WsStatus {
    pub url: String,
    /// Connected and subscribed.
    pub connected: bool,
    /// Times the connection was rebuilt.
    pub reconnects: u64,
    /// Notifications dropped because the receiver fell behind.
    pub lagged: u64,
    /// Highest block height seen in a header.
    pub last_header: Option<u64>,
    /// Last connect or decode error, if any.
    pub last_error: Option<String>,
}

struct Driver<P, S> {
    chain: Chain,
    provider: P,
    sleep: S,
    concurrency: usize,
    speculative_tip: Option<types::CommitState>,
    merged: SelectAll<BoxStream<'static, Input>>,
    kicks: Vec<futures::channel::mpsc::UnboundedSender<()>>,
    status: Arc<Mutex<SourceStatus>>,
    outputs: VecDeque<Result<RawBlockEvents, DexError>>,
    done: bool,
}

impl<P, S, SFut> Driver<P, S>
where
    P: Provider + Clone + 'static,
    S: Fn(Duration) -> SFut + Copy + Send + Sync + 'static,
    SFut: Future<Output = ()> + Send + 'static,
{
    async fn next(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> Option<Result<RawBlockEvents, DexError>> {
        loop {
            if let Some(item) = self.outputs.pop_front() {
                return Some(item);
            }
            if self.done {
                return None;
            }
            let input = self.merged.next().await?;
            let (outputs, actions) = sequencer.push(input);
            for output in outputs {
                match output {
                    Output::Emit(events) => self.outputs.push_back(Ok(events)),
                    Output::Error(err) => {
                        self.outputs.push_back(Err(err));
                        self.done = true;
                    },
                }
            }
            for action in actions {
                self.perform(action);
            }
            self.status.lock().unwrap().cursors = sequencer.cursors();
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Poll(request) => self.merged.push(self.job(request)),
            Action::Timer(duration, tick) => {
                let sleep = self.sleep;
                self.merged.push(
                    stream::once(async move { sleep(duration).await })
                        .map(move |_| Input::Tick(tick))
                        .boxed(),
                );
            },
            Action::Reconnect(src) => {
                if let Some(kick) = self.kicks.get(src) {
                    let _ = kick.unbounded_send(());
                }
            },
        }
    }

    fn job(&self, request: PollRequest) -> BoxStream<'static, Input> {
        let provider = self.provider.clone();
        let chain = self.chain.clone();
        let concurrency = self.concurrency;
        let speculative_tip = self.speculative_tip;
        stream::once(async move {
            let inputs = match request {
                PollRequest::Tips => match poll::tips(&provider, speculative_tip).await {
                    Ok(tips) => vec![Poll::Tips(tips)],
                    Err(err) => vec![failed(request, err)],
                },
                PollRequest::Block(height) => match poll::block(&provider, &chain, height).await {
                    Ok(Some(block)) => vec![Poll::Block(block)],
                    Ok(None) => vec![Poll::Unavailable(request)],
                    Err(err) => vec![failed(request, err)],
                },
                PollRequest::Header(height) => match poll::header(&provider, height).await {
                    Ok(Some(header)) => vec![Poll::Header(header)],
                    Ok(None) => vec![Poll::Unavailable(request)],
                    Err(err) => vec![failed(request, err)],
                },
                PollRequest::Range { from, to } => {
                    range_job(&provider, &chain, from, to, concurrency).await
                },
            };
            stream::iter(inputs.into_iter().map(Input::Poll))
        })
        .flatten()
        .boxed()
    }
}

fn failed(
    request: PollRequest,
    err: crate::error::ProviderError<crate::abi::errors::Exchange::ExchangeErrors>,
) -> Poll {
    // A block served inconsistently (hash mismatch between header and logs)
    // or not available yet is retried; anything else is what the provider's
    // own retry and fallback layers could not recover from.
    let fatal = !matches!(err, crate::error::ProviderError::InvalidRequest(_));
    Poll::Failed { request, error: DexError::Provider(err), fatal }
}

async fn range_job<P: Provider>(
    provider: &P,
    chain: &Chain,
    from: u64,
    to: u64,
    concurrency: usize,
) -> Vec<Poll> {
    let request = PollRequest::Range { from, to };
    let logs = match poll::range(provider, chain, from, to).await {
        Ok(poll::Range::Logs(logs)) => logs,
        Ok(poll::Range::TooLarge) => return vec![Poll::RangeTooLarge { from, to }],
        Err(err) => return vec![failed(request, err)],
    };
    let headers: Vec<_> = stream::iter(from..=to)
        .map(|number| poll::header(provider, number))
        .buffered(concurrency)
        .collect()
        .await;
    let mut collected = Vec::with_capacity(headers.len());
    for header in headers {
        match header {
            Ok(Some(header)) => collected.push(header),
            Ok(None) => return vec![Poll::Unavailable(request)],
            Err(err) => return vec![failed(request, err)],
        }
    }
    match poll::split_range(&logs, collected) {
        Ok(blocks) => {
            let mut inputs: Vec<Poll> = blocks.into_iter().map(Poll::Block).collect();
            inputs.push(Poll::Done(request));
            inputs
        },
        Err(err) => vec![failed(request, err)],
    }
}

#[cfg(test)]
mod tests {
    use alloy::{
        providers::ProviderBuilder,
        rpc::client::RpcClient,
        transports::layers::{RetryBackoffLayer, ThrottleLayer},
    };
    use futures::StreamExt;

    use super::*;

    /// The public mainnet endpoint, throttled to what it allows.
    async fn mainnet_provider() -> impl Provider + Clone + 'static {
        let client = RpcClient::builder()
            .layer(ThrottleLayer::new(10))
            .layer(RetryBackoffLayer::new(10, 100, 200))
            .connect("https://rpc.monad.xyz")
            .await
            .unwrap();
        ProviderBuilder::new().connect_client(client)
    }

    /// Checks the stream contract over a window of live blocks: finalized
    /// heights sequential and unique, speculative batches preceding their
    /// finalized one, tags consistent with the minimum.
    fn check_contract(blocks: &[RawBlockEvents], min: types::CommitState, from: u64) {
        let mut next_final = from;
        let mut spec_seen = std::collections::BTreeSet::new();
        for b in blocks {
            assert!(b.commit_state() >= min, "{b:?} below min {min}");
            assert!(b.block_hash().is_some());
            if b.commit_state() >= types::CommitState::Finalized {
                assert_eq!(b.block_number(), next_final, "finalized out of order");
                next_final += 1;
            } else {
                assert!(b.block_number() >= next_final, "speculative below finalized");
                if !b.supersedes() {
                    assert!(spec_seen.insert(b.block_number()), "duplicate speculative {b:?}");
                }
            }
        }
        assert!(next_final > from, "no finalized block delivered");
    }

    #[tokio::test]
    async fn polling_only_follows_mainnet() {
        let provider = mainnet_provider().await;
        let chain = Chain::mainnet();
        let from = provider
            .get_block_number()
            .await
            .unwrap()
            .saturating_sub(20);
        let (stream, handle) =
            EventSource::new(&chain, provider, types::StateInstant::new(from, 0))
                .min_commit_state(types::CommitState::Voted)
                .build(tokio::time::sleep);
        let blocks: Vec<_> = stream
            .take(40)
            .map(|r| r.expect("stream error"))
            .collect()
            .await;
        check_contract(&blocks, types::CommitState::Voted, from);
        let status = handle.status();
        assert!(status.sources.is_empty());
        assert!(status.cursors.next_finalized > from);
    }

    /// Live subscription against the public mainnet websocket; run with
    /// `cargo test --features ws -- --ignored`.
    #[cfg(feature = "ws")]
    #[tokio::test]
    #[ignore = "needs the public mainnet websocket endpoint"]
    async fn subscription_follows_mainnet() {
        let provider = mainnet_provider().await;
        let chain = Chain::mainnet();
        let from = provider.get_block_number().await.unwrap().saturating_sub(5);
        let (stream, handle) =
            EventSource::new(&chain, provider, types::StateInstant::new(from, 0))
                .min_commit_state(types::CommitState::Proposed)
                .with_ws_url("wss://rpc.monad.xyz")
                .build(tokio::time::sleep);
        let blocks: Vec<_> = stream
            .take(60)
            .map(|r| r.expect("stream error"))
            .collect()
            .await;
        check_contract(&blocks, types::CommitState::Proposed, from);
        let proposed = blocks
            .iter()
            .filter(|b| b.commit_state() == types::CommitState::Proposed)
            .count();
        assert!(proposed > 0, "no Proposed batch from the subscription: {:?}", handle.status());
        let subscribed = blocks.iter().filter(|b| b.block_id().is_some()).count();
        assert!(subscribed > 0, "no batch came from the subscription: {:?}", handle.status());
        let status = handle.status();
        assert!(status.sources[0].connected, "{status:?}");
        assert!(status.sources[0].last_header.is_some());
    }
}
