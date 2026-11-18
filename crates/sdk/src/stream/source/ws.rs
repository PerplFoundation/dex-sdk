//! Websocket subscription source for [`super::EventSource`]: one connection
//! per URL, read in wire order, owned and reconnected by the SDK.
//!
//! The reader speaks JSON-RPC over tokio-tungstenite directly instead of
//! going through alloy's pubsub provider. The sequencer's sealing rules rest
//! on the node's per-connection write order (a block's header, then its
//! `monadLogs`, then its standard `logs`), and a single socket read in order
//! preserves it structurally. alloy fans every subscription out to its own
//! broadcast channel, which loses that order, reconnects and resubscribes
//! silently, which hides exactly the gaps this source must report, and drops
//! notifications when a channel lags; all of it would have to be undone here.
//!
//! Nothing is spawned: the stream holds the socket and reads it when polled,
//! so a slow consumer applies backpressure to the node instead of losing
//! frames (the node closes a client that lags too far, which is a reconnect
//! like any other). Requests are never sent on the socket after the three
//! subscriptions are in place; they would delay notifications.

use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::{
    primitives::{B256, BloomInput},
    rpc::types::{Filter, Header, Log},
    sol_types::SolEventInterface,
};
use futures::{
    FutureExt, SinkExt, StreamExt,
    channel::mpsc::UnboundedReceiver,
    stream::{self, BoxStream},
};
use serde::Deserialize;
use serde_json::value::RawValue;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{self, Message, Utf8Bytes, client::IntoClientRequest},
};

use super::{
    SourceStatus,
    sequencer::{Input, SourceId, Ws},
};
use crate::{Chain, abi::dex::Exchange::ExchangeEvents, stream::RawEvent, types::CommitState};

const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
/// Ping period while the socket is silent; a second silent period in a row
/// declares the connection dead. (The sequencer's watchdog reacts to missing
/// headers much sooner; this only keeps idle connections alive through
/// proxies and catches a half-open socket.)
const KEEPALIVE: Duration = Duration::from_secs(10);

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type Error = Box<dyn std::error::Error + Send + Sync>;
type Timer = Pin<Box<dyn Future<Output = ()> + Send>>;

#[derive(Clone, Copy, Debug)]
pub(super) struct Config {
    /// Subscribe to standard `logs` as the end-of-block marker.
    pub marker: bool,
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

/// Incoming JSON-RPC frame: a response to one of the subscribe requests
/// (`id` set) or a subscription notification (`method`/`params` set).
#[derive(Deserialize)]
struct Frame<'a> {
    id: Option<u64>,
    #[serde(borrow)]
    method: Option<Cow<'a, str>>,
    #[serde(borrow)]
    params: Option<&'a RawValue>,
    #[serde(borrow)]
    result: Option<&'a RawValue>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Notification<'a> {
    #[serde(borrow)]
    subscription: Cow<'a, str>,
    #[serde(borrow)]
    result: &'a RawValue,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Heads,
    Logs,
    Marker,
}

struct Connection {
    socket: Socket,
    /// Subscribe requests whose response is still outstanding.
    wanted: Vec<(u64, Kind)>,
    /// Subscription id, as assigned by the node, per subscription.
    subs: HashMap<String, Kind>,
    keepalive: Timer,
    /// A ping went out and nothing has been received since.
    silent: bool,
    /// Number of connections built before this one.
    epoch: u64,
    /// Connect attempts that failed before this one, carried over to the
    /// next attempt unless this connection delivers.
    attempt: u32,
    /// A header came through: the connection works, the backoff resets.
    delivered: bool,
}

/// TLS client configuration for `wss://` URLs: the bundled roots and the
/// `aws-lc-rs` provider, chosen explicitly so that nothing is installed
/// process-wide (which could change the HTTP provider's TLS behaviour).
fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("aws-lc-rs supports the default TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    Arc::new(config)
}

/// Opens the socket and sends the subscribe requests. Their responses are
/// handled by [`handle`], interleaved with whatever notifications already
/// flow.
async fn connect<S, SFut>(
    url: &str,
    chain: &Chain,
    cfg: Config,
    tls: Arc<rustls::ClientConfig>,
    sleep: S,
    epoch: u64,
) -> Result<Connection, Error>
where
    S: Fn(Duration) -> SFut,
    SFut: Future<Output = ()> + Send + 'static,
{
    let request = url.into_client_request()?;
    let connector = Connector::Rustls(tls);
    let (socket, _) =
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
            .await?;
    let mut conn = Connection {
        socket,
        wanted: Vec::new(),
        subs: HashMap::new(),
        keepalive: Box::pin(sleep(KEEPALIVE)),
        silent: false,
        epoch,
        attempt: 0,
        delivered: false,
    };
    let filter = Filter::new().address(chain.exchange());
    let mut kinds = vec![Kind::Heads, Kind::Logs];
    if cfg.marker {
        kinds.push(Kind::Marker);
    }
    for (id, kind) in (1u64..).zip(kinds) {
        let params = match kind {
            Kind::Heads => serde_json::json!(["monadNewHeads"]),
            Kind::Logs => serde_json::json!(["monadLogs", filter]),
            Kind::Marker => serde_json::json!(["logs", filter]),
        };
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "eth_subscribe",
            "params": params,
        });
        conn.socket
            .send(Message::Text(request.to_string().into()))
            .await?;
        conn.wanted.push((id, kind));
    }
    Ok(conn)
}

enum Phase {
    Connect { attempt: u32, epoch: u64 },
    Connected(Box<Connection>),
}

struct State<S> {
    src: SourceId,
    url: String,
    chain: Chain,
    cfg: Config,
    sleep: S,
    kick: UnboundedReceiver<()>,
    status: Arc<Mutex<SourceStatus>>,
    tls: Arc<rustls::ClientConfig>,
    phase: Phase,
    pending: VecDeque<Input>,
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
        tls: tls_config(),
        phase: Phase::Connect { attempt: 0, epoch: 0 },
        pending: VecDeque::new(),
    };
    stream::unfold(state, |mut st| async move {
        loop {
            if let Some(input) = st.pending.pop_front() {
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
                    let connect =
                        connect(&st.url, &st.chain, st.cfg, st.tls.clone(), st.sleep, epoch);
                    match connect.await {
                        Ok(conn) => {
                            st.phase = Phase::Connected(Box::new(Connection { attempt, ..conn }))
                        },
                        Err(err) => {
                            st.with_status(|s| s.last_error = Some(err.to_string()));
                            st.phase = Phase::Connect { attempt: attempt.saturating_add(1), epoch };
                        },
                    }
                },
                Phase::Connected(mut conn) => {
                    let (input, closed) = match next_event(&mut conn, &mut st.kick, st.sleep).await
                    {
                        Event::Text(text) => handle(&st, &mut conn, &text),
                        Event::Closed(err) => {
                            if let Some(err) = err {
                                st.with_status(|s| s.last_error = Some(err));
                            }
                            (None, true)
                        },
                        Event::Kick => (None, true),
                    };
                    if let Some(input) = input {
                        if let Input::Ws(Ws::Header { height, .. }) = &input {
                            let height = *height;
                            conn.delivered = true;
                            st.with_status(|s| s.last_header = Some(height));
                        }
                        st.pending.push_back(input);
                    }
                    if closed {
                        // Dropping the socket closes the connection; no close
                        // handshake, the peer may be gone already.
                        // A connection that never delivered (a rejected
                        // subscription, an undecodable first frame) backs
                        // off like a failed connect.
                        let epoch = conn.epoch + 1;
                        let attempt =
                            if conn.delivered { 0 } else { conn.attempt.saturating_add(1) };
                        drop(conn);
                        st.with_status(|s| s.connected = false);
                        st.pending.push_back(Input::Ws(Ws::Closed { src: st.src }));
                        st.phase = Phase::Connect { attempt, epoch };
                    } else {
                        st.phase = Phase::Connected(conn);
                    }
                },
            }
        }
    })
    .boxed()
}

enum Event {
    /// A text frame, in wire order.
    Text(Utf8Bytes),
    /// The connection is unusable, with the reason if there is one.
    Closed(Option<String>),
    /// The driver asked for a rebuild.
    Kick,
}

/// Next frame off the socket, with the keepalive and the driver's kick
/// channel serviced in between.
async fn next_event<S, SFut>(
    conn: &mut Connection,
    kick: &mut UnboundedReceiver<()>,
    sleep: S,
) -> Event
where
    S: Fn(Duration) -> SFut,
    SFut: Future<Output = ()> + Send + 'static,
{
    enum Sel {
        Msg(Option<Result<Message, tungstenite::Error>>),
        Tick,
        Kick,
    }
    loop {
        let sel = {
            let msg = conn.socket.next().fuse();
            let tick = (&mut conn.keepalive).fuse();
            let kicked = kick.next().fuse();
            futures::pin_mut!(msg, tick, kicked);
            futures::select! {
                m = msg => Sel::Msg(m),
                _ = tick => Sel::Tick,
                _ = kicked => Sel::Kick,
            }
        };
        match sel {
            Sel::Msg(Some(Ok(msg))) => {
                conn.silent = false;
                match msg {
                    Message::Text(text) => return Event::Text(text),
                    Message::Binary(bytes) => match Utf8Bytes::try_from(bytes) {
                        Ok(text) => return Event::Text(text),
                        Err(e) => return Event::Closed(Some(format!("binary frame: {e}"))),
                    },
                    Message::Close(frame) => {
                        return Event::Closed(Some(match frame {
                            Some(f) => format!("closed by peer: {} {}", f.code, f.reason),
                            None => "closed by peer".to_string(),
                        }));
                    },
                    // Pings are answered by tokio-tungstenite itself.
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {},
                }
            },
            Sel::Msg(Some(Err(e))) => return Event::Closed(Some(e.to_string())),
            Sel::Msg(None) => return Event::Closed(Some("connection ended".to_string())),
            Sel::Tick => {
                if conn.silent {
                    return Event::Closed(Some("no frame since the last ping".to_string()));
                }
                if let Err(e) = conn.socket.send(Message::Ping(Default::default())).await {
                    return Event::Closed(Some(e.to_string()));
                }
                conn.silent = true;
                conn.keepalive = Box::pin(sleep(KEEPALIVE));
            },
            Sel::Kick => return Event::Kick,
        }
    }
}

fn undecodable<S>(st: &State<S>, error: String) -> (Option<Input>, bool) {
    st.with_status(|s| s.last_error = Some(error));
    (Some(Input::Ws(Ws::Undecodable { src: st.src })), true)
}

fn failed<S>(st: &State<S>, error: String) -> (Option<Input>, bool) {
    st.with_status(|s| s.last_error = Some(error));
    (None, true)
}

/// Handles one frame: a sequencer input, if any, and whether the connection
/// must be rebuilt.
fn handle<S>(st: &State<S>, conn: &mut Connection, text: &str) -> (Option<Input>, bool) {
    let frame: Frame = match serde_json::from_str(text) {
        Ok(frame) => frame,
        Err(e) => return undecodable(st, e.to_string()),
    };
    if let Some(id) = frame.id {
        return subscribed(st, conn, id, &frame);
    }
    if frame.method.as_deref() != Some("eth_subscription") {
        return (None, false);
    }
    let note: Notification = match frame.params.map(|p| serde_json::from_str(p.get())) {
        Some(Ok(note)) => note,
        Some(Err(e)) => return undecodable(st, e.to_string()),
        None => return undecodable(st, "notification without params".to_string()),
    };
    match conn.subs.get(note.subscription.as_ref()) {
        Some(kind) => decode(st, *kind, note.result),
        // Not ours (nothing else is subscribed, but be lenient).
        None => (None, false),
    }
}

/// A response to one of the subscribe requests. Once the last one is in,
/// the connection counts as (re)connected.
fn subscribed<S>(
    st: &State<S>,
    conn: &mut Connection,
    id: u64,
    frame: &Frame<'_>,
) -> (Option<Input>, bool) {
    let Some(pos) = conn.wanted.iter().position(|(i, _)| *i == id) else {
        return (None, false);
    };
    let (_, kind) = conn.wanted.remove(pos);
    if let Some(err) = frame.error {
        return failed(st, format!("eth_subscribe: {err}"));
    }
    match frame
        .result
        .map(|r| serde_json::from_str::<String>(r.get()))
    {
        Some(Ok(sub)) => {
            conn.subs.insert(sub, kind);
        },
        Some(Err(e)) => return failed(st, format!("eth_subscribe: {e}")),
        None => return failed(st, "eth_subscribe: response without result".to_string()),
    }
    if !conn.wanted.is_empty() {
        return (None, false);
    }
    let reconnected = conn.epoch > 0;
    st.with_status(|s| {
        s.connected = true;
        if reconnected {
            s.reconnects += 1;
        }
    });
    (reconnected.then_some(Input::Ws(Ws::Reconnected { src: st.src })), false)
}

/// Decodes a notification of the given subscription into a sequencer input.
fn decode<S>(st: &State<S>, kind: Kind, raw: &RawValue) -> (Option<Input>, bool) {
    let src = st.src;
    let ws = |w: Ws| Some(Input::Ws(w));
    match kind {
        Kind::Heads => match serde_json::from_str::<MonadHeader>(raw.get()) {
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
        Kind::Logs => match serde_json::from_str::<MonadLog>(raw.get()) {
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
        Kind::Marker => match serde_json::from_str::<Log>(raw.get()) {
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    static SLEEPS: Mutex<Vec<Duration>> = Mutex::new(Vec::new());

    /// Records reconnect backoffs (keepalives never get to fire here).
    async fn sleep(d: Duration) {
        if d < KEEPALIVE {
            SLEEPS.lock().unwrap().push(d);
        }
        tokio::task::yield_now().await;
    }

    /// A node that rejects every `eth_subscribe` must not be redialled in
    /// a tight loop: the backoff grows until a connection delivers.
    #[tokio::test]
    async fn rejected_subscription_backs_off() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else { return };
                    while let Some(Ok(Message::Text(req))) = ws.next().await {
                        let id =
                            serde_json::from_str::<serde_json::Value>(&req).unwrap()["id"].clone();
                        let reply = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": "no monadNewHeads" },
                        });
                        let _ = ws.send(Message::Text(reply.to_string().into())).await;
                    }
                });
            }
        });
        let status = Arc::new(Mutex::new(super::super::SourceStatus {
            sources: vec![Default::default()],
            ..Default::default()
        }));
        let (_kick_tx, kick_rx) = futures::channel::mpsc::unbounded();
        let source = source(
            0,
            url,
            crate::Chain::testnet(),
            Config { marker: false },
            sleep,
            kick_rx,
            status,
        );
        let closed: Vec<_> = source.take(4).collect().await;
        assert!(
            closed
                .iter()
                .all(|i| matches!(i, Input::Ws(Ws::Closed { .. })))
        );
        let sleeps = SLEEPS.lock().unwrap().clone();
        assert_eq!(
            sleeps,
            vec![BACKOFF_BASE, BACKOFF_BASE * 2, BACKOFF_BASE * 4],
            "{} connections",
            accepted.load(Ordering::SeqCst)
        );
    }

    /// Enabling `ws` must leave rustls able to pick a process-wide provider,
    /// which it cannot when two providers are compiled in.
    #[test]
    fn rustls_default_provider_stays_unambiguous() { let _ = rustls::ClientConfig::builder(); }
}
