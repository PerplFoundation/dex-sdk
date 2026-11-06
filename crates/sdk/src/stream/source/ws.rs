//! Websocket subscription source for [`super::EventSource`]: one connection
//! per URL, owned and reconnected by the SDK.
//!
//! alloy reconnects and resubscribes silently, which hides exactly the gaps
//! this source must know about, so its reconnect hook is refused and every
//! disconnect surfaces as the subscription streams ending. The source then
//! rebuilds the connection itself, with its own backoff, and tells the
//! sequencer about it.
//!
//! The three subscriptions arrive on three channels. The node writes a block's
//! header, then its `monadLogs`, then its standard `logs`, and never
//! interleaves two blocks on a connection, but three channels do not preserve
//! that order by themselves. The reader restores what the sequencer needs by
//! draining the `monadLogs` channel before it hands over anything from the
//! other two: alloy dispatches notifications from a single task in wire
//! order, so once a header or marker has been observed every log dispatched
//! before it is already visible on its channel.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::{
    primitives::{B256, BloomInput},
    providers::{Provider, RootProvider},
    pubsub::{ConnectionHandle, PubSubConnect, RawSubscription},
    rpc::{
        client::ClientBuilder,
        types::{Filter, Header, Log},
    },
    sol_types::SolEventInterface,
    transports::{TransportErrorKind, TransportResult, ws::WsConnect},
};
use futures::{
    FutureExt, StreamExt,
    channel::mpsc::UnboundedReceiver,
    stream::{self, BoxStream},
};
use serde::Deserialize;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use super::{
    SourceStatus,
    sequencer::{Input, SourceId, Ws},
};
use crate::{Chain, abi::dex::Exchange::ExchangeEvents, stream::RawEvent, types::CommitState};

const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
pub(super) struct Config {
    /// Subscribe to standard `logs` as the end-of-block marker.
    pub marker: bool,
    pub channel_size: usize,
}

/// `monadNewHeads` notification: a header with Monad's consensus fields.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonadHeader {
    #[serde(flatten)]
    header: Header,
    block_id: B256,
    commit_state: CommitState,
}

/// `monadLogs` notification: a log with Monad's consensus fields.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonadLog {
    #[serde(flatten)]
    log: Log,
    block_id: B256,
    commit_state: CommitState,
}

/// Connector that refuses alloy's own reconnect, so that a lost connection
/// ends the subscription streams instead of silently resubscribing.
struct NoReconnect(WsConnect);

impl PubSubConnect for NoReconnect {
    fn is_local(&self) -> bool { self.0.is_local() }

    fn connect(&self) -> impl Future<Output = TransportResult<ConnectionHandle>> + Send {
        self.0.connect()
    }

    fn try_reconnect(&self) -> impl Future<Output = TransportResult<ConnectionHandle>> + Send {
        std::future::ready(Err(TransportErrorKind::non_retryable_str(
            "reconnect is handled by perpl-sdk",
        )))
    }
}

struct Subscriptions {
    heads: RawSubscription,
    logs: RawSubscription,
    marker: Option<RawSubscription>,
    /// A header or marker taken off its channel while logs dispatched before
    /// it were still queued; handed over once those are drained.
    stash: Option<Event>,
    // Dropping the provider shuts the connection down.
    _provider: RootProvider,
}

async fn subscribe(url: &str, chain: &Chain, cfg: Config) -> TransportResult<Subscriptions> {
    let connect = NoReconnect(WsConnect::new(url).with_max_retries(0));
    let client = ClientBuilder::default().pubsub(connect).await?;
    let provider = RootProvider::new(client);
    let filter = Filter::new().address(chain.exchange());
    let heads = provider
        .subscribe::<_, MonadHeader>(("monadNewHeads",))
        .channel_size(cfg.channel_size)
        .await?
        .into_raw();
    let logs = provider
        .subscribe::<_, MonadLog>(("monadLogs", filter.clone()))
        .channel_size(cfg.channel_size)
        .await?
        .into_raw();
    let marker = if cfg.marker {
        Some(
            provider
                .subscribe::<_, Log>(("logs", filter))
                .channel_size(cfg.channel_size)
                .await?
                .into_raw(),
        )
    } else {
        None
    };
    Ok(Subscriptions { heads, logs, marker, stash: None, _provider: provider })
}

enum Phase {
    Connect { attempt: u32, epoch: u64 },
    Connected { subs: Box<Subscriptions>, epoch: u64 },
}

struct State<S> {
    src: SourceId,
    url: String,
    chain: Chain,
    cfg: Config,
    sleep: S,
    kick: UnboundedReceiver<()>,
    status: Arc<Mutex<SourceStatus>>,
    phase: Phase,
    pending: Vec<Input>,
}

impl<S> State<S> {
    fn with_status(&self, f: impl FnOnce(&mut super::WsStatus)) {
        if let Some(status) = self.status.lock().unwrap().sources.get_mut(self.src) {
            f(status);
        }
    }
}

/// The subscription source as a stream of sequencer inputs. Never ends.
pub(super) fn source<S, SFut>(
    src: SourceId,
    url: String,
    chain: Chain,
    cfg: Config,
    sleep: S,
    kick: UnboundedReceiver<()>,
    status: Arc<Mutex<SourceStatus>>,
) -> BoxStream<'static, Input>
where
    S: Fn(Duration) -> SFut + Copy + Send + Sync + 'static,
    SFut: Future<Output = ()> + Send + 'static,
{
    let state = State {
        src,
        url,
        chain,
        cfg,
        sleep,
        kick,
        status,
        phase: Phase::Connect { attempt: 0, epoch: 0 },
        pending: Vec::new(),
    };
    stream::unfold(state, |mut st| async move {
        loop {
            if !st.pending.is_empty() {
                let input = st.pending.remove(0);
                return Some((input, st));
            }
            match std::mem::replace(&mut st.phase, Phase::Connect { attempt: 0, epoch: 0 }) {
                Phase::Connect { attempt, epoch } => {
                    if attempt > 0 {
                        let backoff = BACKOFF_BASE
                            .saturating_mul(1u32 << (attempt - 1).min(16))
                            .min(BACKOFF_MAX);
                        (st.sleep)(backoff).await;
                    }
                    match subscribe(&st.url, &st.chain, st.cfg).await {
                        Ok(subs) => {
                            st.with_status(|s| {
                                s.connected = true;
                                if epoch > 0 {
                                    s.reconnects += 1;
                                }
                            });
                            st.phase = Phase::Connected { subs: Box::new(subs), epoch };
                            if epoch > 0 {
                                st.pending.push(Input::Ws(Ws::Reconnected { src: st.src }));
                            }
                        },
                        Err(err) => {
                            st.with_status(|s| s.last_error = Some(err.to_string()));
                            st.phase = Phase::Connect { attempt: attempt.saturating_add(1), epoch };
                        },
                    }
                },
                Phase::Connected { mut subs, epoch } => {
                    let event = next_event(&mut subs, &mut st.kick).await;
                    let (input, closed) = decode(&st, event);
                    if let Some(input) = input {
                        if let Input::Ws(Ws::Header { height, .. }) = &input {
                            let height = *height;
                            st.with_status(|s| s.last_header = Some(height));
                        }
                        if let Input::Ws(Ws::Lagged { n, .. }) = &input {
                            let n = *n;
                            st.with_status(|s| s.lagged += n);
                        }
                        st.pending.push(input);
                    }
                    if closed {
                        drop(subs);
                        st.with_status(|s| s.connected = false);
                        st.pending.push(Input::Ws(Ws::Closed { src: st.src }));
                        st.phase = Phase::Connect { attempt: 0, epoch: epoch + 1 };
                    } else {
                        st.phase = Phase::Connected { subs, epoch };
                    }
                },
            }
        }
    })
    .boxed()
}

enum Event {
    Head(Result<Box<serde_json::value::RawValue>, RecvError>),
    Log(Result<Box<serde_json::value::RawValue>, RecvError>),
    Marker(Result<Box<serde_json::value::RawValue>, RecvError>),
    Kick,
}

/// Next notification, logs first: whatever is queued on the `monadLogs`
/// channel goes before anything from the other channels, see the module
/// docs. A header or marker taken off its channel while logs are queued is
/// stashed until the logs channel is drained.
async fn next_event(subs: &mut Subscriptions, kick: &mut UnboundedReceiver<()>) -> Event {
    loop {
        match subs.logs.try_recv() {
            Ok(v) => return Event::Log(Ok(v)),
            Err(TryRecvError::Lagged(n)) => return Event::Log(Err(RecvError::Lagged(n))),
            Err(TryRecvError::Closed) => return Event::Log(Err(RecvError::Closed)),
            Err(TryRecvError::Empty) => {},
        }
        if let Some(event) = subs.stash.take() {
            return event;
        }
        match subs.heads.try_recv() {
            Ok(v) => {
                subs.stash = Some(Event::Head(Ok(v)));
                continue;
            },
            Err(TryRecvError::Lagged(n)) => return Event::Head(Err(RecvError::Lagged(n))),
            Err(TryRecvError::Closed) => return Event::Head(Err(RecvError::Closed)),
            Err(TryRecvError::Empty) => {},
        }
        if let Some(marker) = subs.marker.as_mut() {
            match marker.try_recv() {
                Ok(v) => {
                    subs.stash = Some(Event::Marker(Ok(v)));
                    continue;
                },
                Err(TryRecvError::Lagged(n)) => return Event::Marker(Err(RecvError::Lagged(n))),
                Err(TryRecvError::Closed) => return Event::Marker(Err(RecvError::Closed)),
                Err(TryRecvError::Empty) => {},
            }
        }
        // Nothing queued: wait for any channel, then start over so that the
        // logs channel is drained first again.
        let taken = {
            let heads = subs.heads.recv().fuse();
            let logs = subs.logs.recv().fuse();
            let marker = async {
                match subs.marker.as_mut() {
                    Some(m) => m.recv().await,
                    None => std::future::pending().await,
                }
            }
            .fuse();
            let kicked = kick.next().fuse();
            futures::pin_mut!(heads, logs, marker, kicked);
            futures::select! {
                r = logs => return Event::Log(r),
                r = heads => Event::Head(r),
                r = marker => Event::Marker(r),
                _ = kicked => return Event::Kick,
            }
        };
        match taken {
            Event::Head(Ok(v)) => subs.stash = Some(Event::Head(Ok(v))),
            Event::Marker(Ok(v)) => subs.stash = Some(Event::Marker(Ok(v))),
            other => return other,
        }
    }
}

fn undecodable<S>(st: &State<S>, error: String) -> (Option<Input>, bool) {
    st.with_status(|s| s.last_error = Some(error));
    (Some(Input::Ws(Ws::Undecodable { src: st.src })), true)
}

/// Decodes a wire event into a sequencer input; the flag says whether the
/// connection must be rebuilt.
fn decode<S>(st: &State<S>, event: Event) -> (Option<Input>, bool) {
    let src = st.src;
    let ws = |w: Ws| Some(Input::Ws(w));
    match event {
        Event::Kick => (None, true),
        Event::Head(Err(e)) | Event::Log(Err(e)) | Event::Marker(Err(e)) => match e {
            RecvError::Lagged(n) => (ws(Ws::Lagged { src, n }), true),
            RecvError::Closed => (None, true),
        },
        Event::Head(Ok(raw)) => match serde_json::from_str::<MonadHeader>(raw.get()) {
            Ok(h) => {
                let may_have_logs = h
                    .header
                    .inner
                    .logs_bloom
                    .contains_input(BloomInput::Raw(st.chain.exchange().as_slice()));
                (
                    ws(Ws::Header {
                        src,
                        height: h.header.inner.number,
                        hash: h.header.hash,
                        parent: h.header.inner.parent_hash,
                        id: h.block_id,
                        state: h.commit_state,
                        timestamp: h.header.inner.timestamp,
                        may_have_logs,
                    }),
                    false,
                )
            },
            Err(e) => undecodable(st, e.to_string()),
        },
        Event::Log(Ok(raw)) => match serde_json::from_str::<MonadLog>(raw.get()) {
            Ok(l) => match ExchangeEvents::decode_log(&l.log.inner) {
                Ok(decoded) => (
                    ws(Ws::Log {
                        src,
                        height: l.log.block_number.unwrap_or_default(),
                        hash: l.log.block_hash.unwrap_or_default(),
                        id: l.block_id,
                        state: l.commit_state,
                        event: Box::new(RawEvent::new(
                            l.log.transaction_hash.unwrap_or_default(),
                            l.log.transaction_index.unwrap_or_default(),
                            l.log.log_index.unwrap_or_default(),
                            decoded.data,
                        )),
                    }),
                    false,
                ),
                Err(e) => undecodable(st, e.to_string()),
            },
            Err(e) => undecodable(st, e.to_string()),
        },
        Event::Marker(Ok(raw)) => match serde_json::from_str::<Log>(raw.get()) {
            Ok(l) => (
                ws(Ws::Marker {
                    src,
                    height: l.block_number.unwrap_or_default(),
                    hash: l.block_hash.unwrap_or_default(),
                }),
                false,
            ),
            Err(e) => undecodable(st, e.to_string()),
        },
    }
}
