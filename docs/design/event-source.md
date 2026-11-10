# `stream::EventSource`: commit-state-aware exchange event stream

Status: implemented in perpl-sdk 0.3.0 (`crates/sdk/src/stream/source/`, `state::Exchange::apply_events_at`,
`stream::trade` journal, CLI flags). This document records the requirements, the facts about the
Monad node and alloy the implementation relies on, the measurements behind its defaults, and the
design itself. Section 8 maps the design onto the code.

Requirements, as stated:

1. A stream like `stream::raw` that consumes `monadLogs` subscriptions where available,
   multiplexing several sources (earliest per block and commit state wins).
2. A minimum commit state to consume (`Proposed`, `Voted`, `Finalized`; `Voted` is not guaranteed
   to be announced by the node), producing that state's updates and the `Finalized` ones, each
   batch tagged with its commit state.
3. Gaps (start to tip, websocket reconnects) filled by polling, so the stream is always consistent,
   sequential and delivered exactly once.
4. A variant of `Exchange::apply_events` that commits only at a chosen commit state and still
   produces state events for lower states.
5. Full backward compatibility: existing code keeps working unchanged and can adopt the features
   gradually.

Measurements were taken on 2026-11-05/06 with a raw websocket frame reader (Python, one connection,
frames timestamped in wire order) and alloy-based probes, against the public endpoints and a
self-hosted mainnet node; node facts cite source lines of the node repositories.

## 0. Verified facts the design relies on

### Monad RPC node (read from source; full notes in monad-rpc-report.md)

Sources analysed (local checkouts, 2026-11-05):

| Repo | Ref | Paths cited below |
|---|---|---|
| `github.com/category-labs/monad-bft` | tag **v0.16.5**, commit `33d9d5c0` (2026-09-29) | `monad-rpc/src/websocket/handler.rs`, `monad-rpc/src/event/{server,mod}.rs`, `monad-rpc/src/data/mod.rs`, `monad-rpc/src/types/eth_json.rs`, `monad-rpc/src/cli.rs`, `monad-triedb-utils/src/triedb_env.rs` |
| `github.com/category-labs/monad` (execution + `monad-exec-events`) | report read at `e8f793d1` (v0.16.6-5, 2026-10-16); cited lines re-verified on local working tree `2d8a6a00` (v0.16.6-73, 2026-11-05). monad-bft v0.16.5 pins the submodule at `a3b16af1a` | `rust/crates/monad-exec-events/src/block_builder/commit_state/mod.rs`, `rust/crates/monad-exec-events/src/block.rs`, `category/execution/monad/event/record_consensus_events.cpp`, `category/execution/runloop/runloop_monad.cpp` |

`bft:` and `exec:` below prefix paths in those two repos.

* Subscription kinds: `newHeads`, `logs`, `monadNewHeads`, `monadLogs`
  (`bft:monad-rpc/src/types/eth_json.rs:465-478`; subscribe handler
  `bft:monad-rpc/src/websocket/handler.rs:413-523`). `monadLogs` honours `address`, `topics`,
  `blockHash`, numeric `fromBlock`/`toBlock`. Bloom pre-check, then per-log match
  (`apply_logs_filter`, `handler.rs:371-403`).
* Single source: the execution event ring, read by one task (`bft:monad-rpc/src/event/server.rs:55-110`)
  and assembled per commit state by `exec:…/block_builder/commit_state/mod.rs:81-273`. One `Block`
  event per (block, commitState) carrying the full header and all logs (`server.rs:136-187`), on a
  broadcast channel of 32 (`bft:monad-rpc/src/event/mod.rs:22`). Every state **re-sends the full
  payload**.
* **Per connection, writes are strictly ordered and never interleave across events**
  (`bft:monad-rpc/src/websocket/handler.rs:294-368`, one event handled to completion per loop
  iteration `:175-291`): for each event, all `monadNewHeads` frames (`:312-318`), then `newHeads`
  (Proposed only, `:320-328`), then every `monadLogs` subscription's full log run in log-index
  order (`:332-345`; index is block-wide, `exec:…/monad-exec-events/src/block.rs:187-209`), then
  `logs` (Proposed only, `:347-363`). Events of different blocks/states do interleave with each other.
  Confirmed on the wire with a raw-frame client: 990 events, 40,821 frames, zero violations.
* Commit-state semantics are *execution* progress: Proposed = this node executed the proposal;
  Voted = execution started a child carrying the QC; Finalized/Verified from consensus
  (`exec:category/execution/runloop/runloop_monad.cpp:579-600` finalization walk, `:731-746`
  Finalized+Verified records). Voted can be skipped: execution drops the QC when the voted height
  is already ≤ finalized (`exec:category/execution/monad/event/record_consensus_events.cpp:50-55`)
  and the builder accepts Finalized straight from Proposed (`commit_state/mod.rs:199-205`).
  Verified is not guaranteed (`commit_state/mod.rs:236`, `:261-263`). A block is always first
  *built* as Proposed (`commit_state/mod.rs:105-141`; later states for unknown blocks are dropped,
  `:158-167`, `:189-197`), but a connection that joins late first sees a later state.
* **No notification for abandoned proposals** (removed in `commit_state/mod.rs:209-225`, only a
  debug log in `bft:monad-rpc/src/event/server.rs:119-124`); no `removed: true`. `blockId` =
  consensus block id (`server.rs:141`; blake3 of the consensus header,
  `bft:monad-consensus-types/src/block.rs:187-191`); `blockHash` = execution eth hash
  (`exec:…/monad-exec-events/src/block.rs:89`), also in every log (`:173-176`). Unrelated values;
  `blockId` is the fork key. Observed over 4.5 min of mainnet: no height with two ids.
* **A lagging connection is closed** (code 1011 "ws server lagging", `handler.rs:272-279`), as
  is every connection on a ring gap ("websocket server gapped", `handler.rs:301-305`; gap detection
  `server.rs:66-73`, `:89-96`; builder reset: blocks in flight never get later states). Any RPC
  request sent *on the subscription socket* is awaited inside the same loop (`handler.rs:583-609`)
  and delays notifications, which can trigger the lag close. Server pings every 20 s, closes after
  60 s idle (`handler.rs:57`, `:177-188`). Limits: 500 connections (`:83-91`), 100 subscriptions
  per connection (`:464-492`), 25 MB per notification (`:625-641`); defaults in
  `bft:monad-rpc/src/cli.rs:56-74`.
* Polling: `latest`/`pending` = latest proposed, `safe` = voted, `finalized` = finalized
  (`bft:monad-rpc/src/types/eth_json.rs:373-376`, resolution `bft:monad-rpc/src/data/mod.rs:102-111`
  via triedb metadata polled every 5 ms, `bft:monad-triedb-utils/src/triedb_env.rs:97`). The block
  buffer (1024 blocks, `bft:monad-rpc/src/main.rs:326-353`) serves Proposed blocks by number and
  *replaces* them in place on a fork (`bft:monad-rpc/src/data/buffer/mod.rs:134-177`). `eth_getLogs`
  serves Proposed blocks (`data/mod.rs:695-706`); a number above latest returns -32602 "Block
  requested not found" (`triedb_env.rs:932-950`; already mapped to `ProviderError::InvalidRequest`);
  `toBlock` is capped at latest (`data/mod.rs:570-574`). **The `blockHash` filter resolves hash →
  number and does not verify the served block's hash** (`data/mod.rs:579-621`) — always check each
  log's `blockHash`. Range limit `to - from > max` (`data/mod.rs:628-630`) with node default 1000
  (1001 blocks inclusive; `cli.rs:103-105`);
  both public endpoints (`rpc.monad.xyz`, `rpc-mainnet.monadinfra.com`) allow a span of 100
  (101 blocks) over HTTP and over websocket alike.
* **Public websockets exist and are full-featured**: `wss://rpc.monad.xyz` and
  `wss://rpc-mainnet.monadinfra.com` (path `/`, Cloudflare-fronted; `https://` upgrade on the
  HTTP origin returns 405, which is what misled the first draft). Both accept `monadNewHeads`, `monadLogs`,
  `newHeads`, `logs`, duplicate subscriptions, `eth_unsubscribe`, and plain JSON-RPC calls on the
  same socket (`eth_getLogs` ~200 ms, `finalized` tag works). Server ping every ~20 s. Measured
  60 s each (~45k / ~40k frames, 199 Proposed heights, zero loss, zero gaps): identical write
  order to the private node (`monadNewHeads` → `monadLogs` run → `logs`), standard `logs` count
  equals the `monadLogs` Proposed count on every block, and **less jitter than the private node**
  (header → last marker frame p50 4–7 ms, p99 41–59 ms, max 87 ms; private node p99 855 ms).
* Standard `newHeads`/`logs` fire at Proposed only (`handler.rs:59`, `:320`, `:347`; measured: 100%).

### alloy 2.5.0 pubsub (paths in alloy-pubsub-report.md)

* `Provider::subscribe::<P, R>(params)` sends `params` verbatim as `eth_subscribe` params, so
  `("monadLogs", filter)` → `["monadLogs", {...}]`. Needs cargo feature `pubsub`; `WsConnect`
  needs `provider-ws` (aws-lc-rs) or `provider-ws-ring`. `RpcClient::pubsub_frontend().is_some()`
  is the runtime check; HTTP providers fail `subscribe` with `TransportErrorKind::PubsubUnavailable`.
* **Reconnect is automatic and silent**: default 10 retries, 3 s base, exponential to 30 s
  (~195 s total), then the service task ends and every subscription stream returns `None`.
  Resubscription replays the same `eth_subscribe`; the consumer's stream keeps the same channel
  and gets **no marker**. Notifications sent while disconnected are lost. A resubscribe the server
  rejects leaves the stream silent forever (never ends). Only hook: a custom `PubSubConnect`
  whose `try_reconnect` signals an epoch.
* Delivery is a tokio broadcast channel per subscription, **default capacity 16**; the `into_stream`
  adapters **drop lagged items silently**; `recv()` on `into_raw()` returns `Lagged(n)`.
  Capacity via `GetSubscription::channel_size(n)`. (A first probe run with `into_stream` "lost"
  commit states; with a 1 M channel and raw `recv` nothing was lost.)
* Keepalive ping every 10 s by default; a missed pong kills the connection (then reconnect).

### Live measurements (mainnet, Perpl exchange ≈ 42 logs/block, block interval p50 ≈ 300 ms)

| Metric | Value |
|---|---|
| Proposed header → end of its `monadLogs` run (first standard `logs` frame), private node | p50 5 ms, p90 107 ms, max 614 ms |
| Same, public `wss://rpc.monad.xyz` / `wss://rpc-mainnet.monadinfra.com` (last marker frame) | p50 4 / 7 ms, p99 41 / 59 ms, max 87 / 87 ms |
| Quiescent seal re-emits at Q = 10 ms, private vs public | 64/248 vs 0–1/199 |
| Proposed header → next header of any event on the connection | p50 223 ms |
| Proposed → Voted header (same block) | p50 287 ms |
| Voted → Finalized header | p50 260–280 ms |
| Finalized → Verified header | p50 0.9 s |
| HTTP tips (`latest`, `safe`, `finalized`) | `safe` = latest−1, `finalized` = latest−2, consistently |
| Blocks with all four header states | private 243/248, 281/286, 292/297; public 194/199 ×2 (rest: first/last blocks of the window) |
| Heights with competing proposals / `removed` logs | 0 / 0 |

### Snapshot clone benchmark (requirement 4 feasibility)

Live all-positions mainnet snapshot: 16 perpetuals, 346 accounts, 520 positions, 638 orders,
615 book levels; built in 23 s over the public RPC at 15 rps. Mainnet has 5,593 accounts in total.

| Accounts in snapshot | `Exchange::clone()` p50 | apply empty block |
|---|---|---|
| 346 (live) | 55 µs | 1.3 µs |
| 1,000 (synthetic) | 166 µs | 3 µs |
| 5,000 | 237 µs | 1.5 µs |
| 20,000 | 257 µs | 0.6 µs |
| 100,000 | 862 µs | 0.6 µs |
| 200,000 | 1.7 ms | 0.6 µs |

Clone cost is linear and tiny next to a 300 ms block interval even at 40× today's account count.
The Arc/copy-on-write idea from the first draft is dropped; the overlay can simply be rebuilt by cloning.

## 1. Public API (all additive)

### 1.1 `types::CommitState`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommitState { Proposed, Voted, Finalized, Verified }
```
`Ord` is consensus order. The stream never emits `Verified`; it is normalised to `Finalized`.
`block_tag()` maps to `latest`/`safe`/`finalized`; `Display`/`FromStr` use the lowercase names
(`latest`/`safe` accepted as aliases) so the CLI can take it as a flag.

### 1.2 Block metadata on `types::BlockEvents<T>`

```rust
pub struct BlockEvents<T> {
    instant: StateInstant,
    events: Vec<T>,
    // new, defaulted, so `BlockEvents::new` keeps its signature
    commit_state: CommitState,     // default Finalized ("treat as committed")
    block_hash: Option<B256>,      // execution block hash; None for hand-built batches
    block_id: Option<B256>,        // Monad consensus block id; only from subscriptions
    supersedes: bool,              // default false
}
// builder-style setters + accessors: with_commit_state, with_block_hash, with_block_id,
// superseding, commit_state(), block_hash(), block_id(), supersedes(), block_number();
// map_events(|events| ..) and with_tags_of(&other) keep the tags when deriving batches
```
Tags live on `BlockEvents` itself, so the new stream's item type is still `RawBlockEvents`
(`stream::trade`, `apply_events`, user code compile unchanged) and `StateBlockEvents` carries the
same tag for free (requirement 2). `stream::raw` starts tagging its output `Voted`, which is what it
already guarantees (it gates on `safe`).

### 1.3 `stream::EventSource` builder

```rust
pub struct EventSource<P> { /* chain, from, polling provider, ws urls, options */ }

impl<P: Provider + Clone + 'static> EventSource<P> {
    /// `provider` is the polling provider, same role as in `stream::raw`. To multiplex several
    /// HTTP endpoints wrap them in alloy's `FallbackLayer` (or any other layer) before passing it.
    pub fn new(chain: &Chain, provider: P, from: StateInstant) -> Self;
    /// Lowest commit state to deliver. Default Voted (same as `stream::raw`).
    pub fn min_commit_state(self, s: CommitState) -> Self;
    /// Websocket subscription source (feature `ws`). Repeatable: every URL gets its own
    /// connection and the sequencer takes the earliest arrival per (block, commit state). The SDK
    /// owns the connections, their reconnects and backoff; the websocket is never used for
    /// polling. Without any URL the source runs in pure polling mode.
    #[cfg(feature = "ws")] pub fn with_ws_url(self, url: impl Into<String>) -> Self;
    /// When a Proposed block is considered complete (§2.4). Default `SealPolicy::Marker`.
    pub fn seal_policy(self, p: SealPolicy) -> Self;
    /// `eth_getLogs` span per gap-fill call. Default 100 (public RPC limit; node default 1000).
    pub fn poll_chunk(self, blocks: u64) -> Self;
    /// Concurrent header fetches during gap fill. Default 8.
    pub fn poll_concurrency(self, n: usize) -> Self;
    /// No header from any subscription for this long ⇒ tips re-check. Default 500 ms
    /// (≈ 1.7 block intervals; block interval p50 ≈ 300 ms, max observed ≈ 1.7 s).
    pub fn watchdog(self, d: Duration) -> Self;
    /// Period of the unconditional finalized-tip check. Default 500 ms (2 rps on the polling
    /// provider; fits the CLI's 15 rps throttle next to gap fills).
    pub fn tip_interval(self, d: Duration) -> Self;
    /// Broadcast channel size for subscriptions. Default 4096 (alloy's default is 16).
    pub fn channel_size(self, n: usize) -> Self;
    pub fn build<S, SFut>(self, sleep: S)
        -> (impl Stream<Item = Result<RawBlockEvents, DexError>> + Send + 'static, SourceHandle)
    where S: Fn(Duration) -> SFut + Copy + Send + Sync + 'static, SFut: Future<Output = ()> + Send + 'static;
}

pub enum SealPolicy {
    /// Exact. A Proposed block is complete when the first standard `logs` frame for it arrives on
    /// the same connection (the node writes `monadLogs` then `logs` per event), or when a later
    /// header arrives, or when the header's bloom excludes the exchange. ≈ +5 ms p50.
    Marker,
    /// Exact, no extra subscription. Complete when the next header (any block, any state) arrives
    /// on the connection. ≈ +223 ms p50 (about the same as waiting for Voted).
    NextHeader,
    /// Heuristic. Complete when no log arrived for `quiet` after the header; a late log re-emits
    /// the block as superseding. Measured: Q=10 ms ⇒ 7–15% re-emits. Opt-in only.
    Quiescent { quiet: Duration },
}

pub struct SourceHandle { /* Arc<Mutex<SourceStatus>> */ }
impl SourceHandle {
    pub fn status(&self) -> SourceStatus;
}
pub struct SourceStatus { pub sources: Vec<WsStatus>, pub cursors: Cursors }
pub struct WsStatus { url, connected, reconnects, lagged, last_header, last_error }
pub struct Cursors { next_finalized, next_speculative, tips: Option<Tips>, poll_chunk }
```

Sources (an earlier draft took a `Vec<P>` with roles inferred from `pubsub_frontend()`; dropped, a
caller-supplied subscription provider adds nothing the SDK cannot do better itself and cannot be
rebuilt by the SDK):
* **One polling provider** `P`, exactly like `stream::raw`. All `eth_getLogs`, header and tip
  requests go through it. Multiplexing or failover of HTTP endpoints is the caller's layering
  (alloy `FallbackLayer`, the CLI's existing `ThrottleLayer` + `RetryBackoffLayer`); the SDK does
  not re-implement it. An `Err` from it after the layer stack gives up ends the stream.
* **Zero or more websocket URLs** (`with_ws_url`, feature `ws`): one connection each, subscriptions
  `monadNewHeads`, `monadLogs{address}` and (Marker policy) `logs{address}`, read via
  `into_raw().recv()` so `Lagged` is observed. Requests are never sent on these sockets (they
  delay notifications and can get the connection closed for lagging). Earliest arrival per
  `(height, hash, state)` wins across connections; the rest is deduplicated.
* No URL ⇒ pure polling mode (still honours `min_commit_state`, still range-based catch-up, so it
  is a straight improvement over `raw`). This mode needs no `pubsub`/`ws` cargo features.
* `sleep` stays the runtime-agnostic timer; nothing is spawned; the whole thing is one `Stream`
  over `select_all` of raw subscription receivers, a `FuturesUnordered` of poll jobs and timers.
  Same cancellation-safety caveat as `raw`.

### 1.4 Adapters for gradual adoption

```rust
/// Only Finalized updates: exactly-once per height, strictly sequential, never superseded.
pub fn finalized(s) -> impl Stream<Item = Result<RawBlockEvents, DexError>>;
/// First update per height at ≥ min: exactly-once per height. Yields
/// `DexError::BlockSuperseded(height)` if a superseding update arrives for an emitted height
/// (possible only for min = Proposed; a fork was never observed at Voted in practice).
pub fn once(s) -> impl Stream<Item = Result<RawBlockEvents, DexError>>;
```

### 1.5 Stream contract

Let `min` be the configured minimum. For every height `h ≥ from.block_number()`:

1. **Finalized layer.** Exactly one update tagged `Finalized`, strictly sequential in `h`, never a
   gap, never a duplicate, never superseded. Content = the finalized block's logs.
2. **Speculative layer** (only if `min < Finalized`). At most one update per `(h, block_hash)`,
   tagged with the first observed state `s` with `min ≤ s < Finalized`. If the first chance to emit
   `h` is already Finalized (gap fill, late join, Voted skipped with `min = Voted`), there is no
   speculative update for `h`; the Finalized one satisfies `min`.
3. **Speculative updates are sequential modulo rewinds.** An update with `supersedes == true` at
   height `h` invalidates every earlier speculative update with height `≥ h`; the sequence resumes
   at `h+1`. Causes: a second proposal at `h` (different id/hash), or a late log under
   `Quiescent` (same hash). Under `Marker`/`NextHeader` only the fork case exists.
4. A Finalized update whose hash differs from the speculative hash emitted for `h` is also tagged
   `supersedes == true`.
5. Speculative updates for `h` precede the Finalized update for `h`.
6. `Err` items are non-recoverable (the polling provider failed after its own retry/fallback
   layers, decode error). Everything
   transient is retried internally; subscription loss degrades to polling and is visible through
   `SourceHandle::status()`.

## 2. Internals

```
 ws url(s) ──────▶ raw recv loop per connection: monadNewHeads · monadLogs · logs ─┐  Input
                                                                                   ▼
 provider ───────▶ poll jobs: tips · range logs · block logs · headers ──────▶ ┌────────────┐
                                                                            │ Sequencer  │──▶ RawBlockEvents
 timers ─▶ watchdog · tip_interval ─────────────────────────────────────────▶│ (pure)     │
                                                                            └────────────┘
```
The sequencer is pure and synchronous (`push(Input) -> Vec<Output>`, `wants() -> Vec<PollRequest>`),
like `TradeProcessor` in `stream/trade.rs`, so every scenario below is a scripted unit test.

### 2.1 Inputs

* `Ws::Header { src, height, hash, parent, id, state, timestamp, may_have_logs }` (bloom checked
  by the reader), `Ws::Log { src, height, hash, id, state, event }`, `Ws::Marker { src, height,
  hash }` (standard `logs` frame), `Ws::Lagged { src, n }`, `Ws::Closed { src }`,
  `Ws::Reconnected { src }`, `Ws::Undecodable { src }`.
* `Poll::Tips(Tips)`, `Poll::Block(PolledBlock)` (hash-verified: every log's `blockHash ==
  header.hash`, else the job fails non-fatally and is retried), `Poll::Header(BlockHeader)`,
  `Poll::Unavailable(req)`, `Poll::RangeTooLarge { from, to }`, `Poll::Done(req)`,
  `Poll::Failed { req, error, fatal }`.
* `Tick::Tip`, `Tick::Watchdog`, `Tick::Quiet { src, height, hash, token }`.
* Outputs: `Action::Poll(PollRequest::{Tips, Block(h), Header(h), Range { from, to }})`,
  `Action::Timer(Duration, Tick)`, `Action::Reconnect(src)`; `Output::Emit(RawBlockEvents)`,
  `Output::Error(DexError)` (ends the stream).

### 2.2 State

```
next_final: u64                      // next height to emit at Finalized
final_hash: Option<B256>             // hash of the last Finalized emission: the chain anchor
spec_next: u64                       // next height to emit speculatively
spec_emitted: BTreeMap<u64, B256>    // speculative heights ≥ next_final and the hash emitted
pending_rewind: HashSet<u64>         // emitted heights that got a competing proposal
candidates: BTreeMap<u64, HashMap<B256 /*hash*/, Candidate>>
Candidate { seq, id: Option<B256>, state: CommitState, header: Option<{timestamp, parent}>,
            logs: BTreeMap<u64 /*log_index*/, RawEvent>, logs_complete: bool,
            log_token: u64 /*Quiescent*/, dirty: bool /*late log after emission*/ }
            // complete() = logs_complete && header.is_some()
per source: run: Option<(height, hash, state)>   // monadLogs run streaming on that connection
            headers_seen, stale periods, alive
tips: Option<Tips>, in_flight: HashSet<PollRequest>, chunk: u64
```

### 2.3 Multiplexing ("earliest wins")

Candidates are keyed by `(height, hash)`. The first source to deliver a header or a log populates
the candidate; later deliveries (another source, or the same block at a higher state) are no-ops
for content and only raise `state = max(state, incoming)`. Logs are keyed by `log_index`, so
duplicates are idempotent. Each connection has its own ordering, so the current `run` is tracked
per source; completeness (§2.4) is decided per source and the first source to complete the block
wins.

### 2.4 Completeness (sealing)

Per connection the node writes `monadNewHeads → newHeads → monadLogs run → logs run` per event
and never interleaves two events (`bft:monad-rpc/src/websocket/handler.rs:312-363`). Standard
`newHeads` is written *before* the `monadLogs` run, so it cannot serve as the end marker; verified
on `wss://rpc.monad.xyz` (99/99 Proposed blocks in that order, `newHeads` → first `monadLogs`
frame p50 32 µs). Only the standard `logs` subscription follows the run, hence `Marker` uses `logs`.

alloy, however, delivers the three subscriptions on three separate broadcast channels, so the wire
order is not observable across them directly. The reader (`ws.rs`) restores what the sequencer
needs: it **drains the `monadLogs` channel before handing over anything from the header or marker
channel** (a header or marker taken off its channel while logs are queued is stashed until the
logs channel is empty). alloy dispatches notifications from a single task in wire order, so once a
header or marker has been observed, every log dispatched before it is already visible on its
channel. Logs dispatched *after* the header may be handed over before it; the rules below tolerate
that. A run is identified by `(height, hash, state)`, since every state re-sends the full run.

Rules, per source (`Sequencer::on_ws`):

* **R1**: a log of another run ends the current run (the node streams one run contiguously).
* **R2**: a header ends the current run unless the header is the run's own (its logs were
  dispatched after it and drained before it).
* **R3**: a Proposed `logs` marker ends the run of its block, provided at least one log of that
  block was seen (a marker for a run that started before the subscription proves nothing).
* **R4**: a header whose logs bloom excludes the exchange address needs no logs.
* **R5**: a lost connection (close, lag, undecodable frame, watchdog) ends nothing; the block stays
  incomplete until another source or a poll completes it. Lag is handled by rebuilding the
  connection, as the node itself does with a lagging client; the channel is 4096 deep per
  subscription, so lag means the consumer stalled for seconds.
* **R6**: `Quiescent` ends the current run when no log arrived for `quiet` after the last one
  (timers carry a per-candidate token so stale timers are ignored); a late log after emission marks
  the candidate dirty and it is re-emitted as superseding once quiet again.

`Marker` = R1–R5 with R3; `NextHeader` = R1, R2, R4, R5 (no `logs` subscription); `Quiescent`
adds R6. Polled blocks are always complete. A candidate is emittable when its logs are complete
*and* its header is known (timestamp, parent).

### 2.5 Emission (`drain` after every input)

Order per drain: rewinds, speculative, finalized, then speculative again (finalizing a height can
unblock the speculative heights above it).

Speculative layer (`min < Finalized`):
```
loop:
  h = max(spec_next, next_final)
  cand = complete candidate at h with min ≤ state < Finalized, whose parent hash equals the hash
         emitted at h-1 (spec_emitted[h-1], or final_hash when h == next_final; unknown parents pass),
         highest state first, most recently seen on ties
  if none: break
  emit(cand, tag = cand.state, supersedes = false); spec_emitted[h] = hash; spec_next = h + 1
```
The parent check is what keeps a stale descendant of an abandoned proposal from being re-emitted
after a rewind, without tracking fork trees explicitly.

Rewind: a candidate created at a height already in `spec_emitted` with a different hash is noted in
`pending_rewind`; once complete it is emitted with `supersedes = true`, `spec_emitted` is truncated
below `h`, and `spec_next = h + 1`. (`Quiescent` late log: same hash, `dirty` flag, same effect.)

Finalized layer:
```
loop:
  cand = candidate at next_final with state ≥ Finalized (Verified counts)
  if none: break
  if !cand.logs_complete: request Poll::Block(next_final) if not in flight; break
  supersedes = spec_emitted.get(h).is_some_and(|x| x != hash)
  emit(cand, Finalized, supersedes); spec_emitted.remove(..=h); candidates.remove(..=h)
  next_final = h + 1; final_hash = hash
  if supersedes { spec_emitted.clear(); spec_next = h + 1 } else { spec_next = max(spec_next, h + 1) }
```
A height known only speculatively (polled while above the finalized tip, or subscribed at a lower
state) is confirmed, once the tips say it is finalized, by a header-only fetch when its candidate is
complete, and by a block fetch otherwise.

### 2.6 Progress guarantees and gap detection

The Finalized layer must never depend on the subscription alone, because the node drops in-flight
blocks on a ring gap and alloy loses everything sent while reconnecting. So:
* **Unconditional tip check every `tip_interval`** (`eth_getBlockByNumber(finalized)` on the
  polling provider, plus the tip of the minimum state when there is no subscription and
  `min < Finalized`; 500 ms default ≈ 1–2 blocks, 1–2 rps). If `tips.finalized ≥ next_final` and
  the candidate is missing or incomplete ⇒ range poll `[next_final, min(tips.finalized,
  next_final + chunk - 1)]`, one range in flight at a time; a single missing block is fetched
  directly.
* **Watchdog**: no header on any source for `watchdog` ⇒ tips check now; the source is marked
  stale; after 6 consecutive periods (3 s without a header) the connection is torn down and
  rebuilt; polling continues meanwhile.
* **Speculative gap fill**: on every tips result, heights in `(tips.finalized, tips.at(min)]`
  from `spec_next` up with no candidate are polled per block, at most `poll_concurrency` at a time,
  tagged by comparison with the tips (conservative: a stale tip only understates). With a live
  subscription those candidates already exist by the time the tick fires, so nothing is polled.
* **Lag** (`Lagged(n)` from the raw receiver), **undecodable frames** and **close** ⇒ the run in
  flight on that source stays incomplete, a tips check is requested, and the connection is rebuilt
  (`Ws::Closed` then `Ws::Reconnected`).
* Start-up: `[from, tips.finalized]` by range polls in `poll_chunk` spans (the node allows 1001,
  the public RPC 101; the chunk is clamped down automatically on "block range too large" and
  remembered), headers fetched per height with `poll_concurrency`; `[finalized+1, latest]` per-block
  polls if `min < Finalized`. Subscription data buffers meanwhile and takes over seamlessly.
* Pruning: everything below `next_final` is dropped on Finalized emission, abandoned proposals
  included. Memory is bounded by the finality lag (2 blocks) plus `tip_interval` worth of slack.

### 2.7 Subscription lifecycle

* Every websocket connection is SDK-owned. The SDK wraps `WsConnect` in a `PubSubConnect`
  whose `try_reconnect` **refuses** (returns `TransportErrorKind::NonRetryable`, which the
  service loop treats as "shut down now"), so alloy's silent
  resubscribe never happens: every disconnect ends the subscription streams, the source emits
  `Ws::Closed`, and the SDK rebuilds the provider and resubscribes with its own backoff
  (`sleep`-based, 250 ms base, exp to 10 s, unbounded), emitting `Ws::Reconnected { epoch }`.
  Reconnect and gap handling are therefore explicit and uniform: closed → tips check → range
  poll for anything the subscription missed. (Alternative kept in mind: `try_reconnect` that
  bumps an epoch and delegates; same observability, but then alloy's own bounded retry schedule
  applies and the stream can still end. Refusing is simpler.)
* `channel_size` applied on every `subscribe`; the raw receiver loop forwards `Lagged` and
  `Closed`. A connection rebuilt because of a watchdog verdict (`Action::Reconnect`, delivered to
  the reader over a channel) counts as a reconnect too. Lag and undecodable frames rebuild the
  connection as well (§2.4 R5).
* The polling provider is never used for subscriptions and websockets never for requests.

## 3. `state::Exchange` changes (requirement 4)

```rust
impl Exchange {
    /// Unchanged: always commits (legacy).
    pub fn apply_events(&mut self, events: &RawBlockEvents) -> Result<Option<StateBlockEvents>, DexError>;

    /// Commits `events` iff `events.commit_state() >= min`; otherwise applies them to a
    /// speculative overlay and returns the state events without touching the snapshot.
    pub fn apply_events_at(&mut self, events: &RawBlockEvents, min: CommitState)
        -> Result<Option<StateBlockEvents>, DexError>;

    pub fn instant(&self) -> StateInstant;                 // committed, unchanged
    pub fn block_hash(&self) -> Option<B256>;              // last committed block's hash
    pub fn speculative(&self) -> Option<&Exchange>;        // overlay view (Proposed-level book etc.)
    pub fn latest(&self) -> &Exchange;                     // overlay if any, else self
    pub fn latest_instant(&self) -> StateInstant;     // == instant() without overlay
}
```

Internals: private `spec_applied: VecDeque<RawBlockEvents>` and `spec_state: Option<Box<Exchange>>`
(the overlay, built lazily). The existing `apply_events` body becomes a private `apply_block`;
`apply_events(e)` is `apply_events_at(e, Proposed)`, i.e. commit everything, so legacy callers keep
their behaviour (including the mismatch check, which cannot fire on hand-built batches without a
hash).

Given the clone benchmark, the overlay is **rebuilt, not reconciled**:

```
apply_events_at(u, min):
  if u.supersedes: applied.retain(|b| b.height < u.height); overlay = None   // rebuilt lazily
  if u.state >= min:                                      // commit path
      if u.height <= self.instant.height:
          hashes known and different → Err(CommittedBlockMismatch{height})   // rebuild snapshot
          return Ok(None)
      u.height != self.instant.height+1 → Err(BlockOutOfOrder)
      out = self.apply_block(u)
      // keep only speculative blocks above the new committed height and consistent with it
      match applied.front() { Some(b) if b.height == u.height && b.hash != u.hash => applied.clear(), _ => {} }
      applied.retain(|b| b.height > u.height); overlay = None
      return Some(out tagged u.state)
  else:                                                   // speculative path
      ov = overlay.get_or_insert_with(|| clone of self + replay(applied))   // ≈ 60–250 µs + replay
      if u.height <= ov.state.instant.height: same hash → Ok(None); else treat as supersedes (above)
      u.height != ov.state.instant.height+1 → Err(BlockOutOfOrder)
      out = ov.state.apply_block(u); applied.push_back(u.clone())
      return Some(out tagged u.state)
```
* Replay re-applies ≤ finality-lag blocks (2–3) and discards their state events.
* Cost per committed block: one clone (55 µs today, ≤ 0.3 ms at the full 5.6 k accounts) plus the
  replay, versus a 300 ms block interval. No copy-on-write needed.
* Funding runs independently in both layers and converges since the events are identical.
* New errors `BlockSuperseded(u64)`, `CommittedBlockMismatch(u64)`. `DexError` is not
  `#[non_exhaustive]`, so a downstream exhaustive match breaks: minor bump, changelog note.

`SnapshotBuilder::at_commit_state(CommitState)` maps to `latest`/`safe`/`finalized`. A consumer with
`min = Finalized` should snapshot at `finalized` so the committed snapshot never holds a revertible
block; the stream starts at `exchange.instant().next()` as today.

## 3b. `stream::trade` on multi-level streams

**Not a breaking change, no new function.** `trade(chain, provider, raw_events)` is a per-block
mapper over whatever stream the caller feeds it: today `stream::raw` (one Voted item per height),
tomorrow `EventSource{min: Proposed}` (Proposed → Finalized items, rewinds). Its item type stays
`BlockTrades = BlockEvents<TradeEvent>`, which now carries the tags from §1.2. Existing callers
keep feeding `raw` and see exactly what they see today. Callers who want Proposed trades change
only the input stream. What has to change is inside, so that re-sends and forks are handled:

* **Tags are propagated.** `process_block` builds the output with the input's `commit_state`,
  `block_hash`, `block_id` and `supersedes` (helper `BlockEvents::map_events(|events| ..)` that
  keeps metadata, used by `trade` and by `Exchange` for `StateBlockEvents`). One output item per
  input item, same cadence: a Proposed batch with the trades, later the Finalized batch with the
  *same* trades tagged `Finalized`, and a superseding batch when the source rewinds. Consumers that
  want one item per height put `stream::once`/`stream::finalized` in front, as before.
* **Cross-block state becomes fork-safe.** `TradeProcessor` keeps one map that survives blocks:
  `maker_orders: (perp, order_id) → {client_order_id, builder}`, filled by `OrderPlaced` and read
  at fill time for maker attribution. Everything else (`order_context`, `pending_maker_fills`,
  `prev_tx_index`) is per-transaction scratch. Re-applying the same block is idempotent (same
  inserts), so re-sends are harmless; a *superseded* Proposed block is not: its `OrderPlaced`
  entries would attribute later fills of a reused order id to an order that never existed.
  Cloning the map per block is the wrong tool here (it retains closed orders and can grow to
  65,535 entries per perpetual); an **undo journal** is enough:
  - applying a block tagged `Proposed` records, per overwritten/inserted key, the previous value in
    `journal: VecDeque<(height, hash, Vec<(key, Option<PlacedOrder>)>)>` (a handful of entries per
    block, only `OrderPlaced` writes);
  - a block arriving tagged `≥ Voted` with the same `(height, hash)` as a journal entry promotes it
    (journal entries up to that height are dropped); `Voted` is treated as committed, consistent
    with `raw`, the `safe` tag and `SnapshotBuilder`'s default, so legacy input never journals;
  - a block with `supersedes == true` at height `h` undoes every journal entry with height `≥ h`
    in LIFO order, then applies normally;
  - a committed block whose hash differs from a journal entry at the same height is treated the
    same way (undo `≥ h`, then apply) and is logged; it cannot happen with `EventSource` output
    (rewinds are always announced) but keeps the processor total.
* **Drive-by fix.** Transaction scratch is reset at the start of every `process_block`; today the
  reset keys on `tx_index` increasing, so the last transaction's `order_context` can leak into the
  first transaction of the next block (and, with re-sends, of the same block). Harmless for
  well-formed transactions, wrong in principle, and trivially fixed.
* **`NormalizationConfig`** is fetched once at `latest`, unchanged. A perpetual listed mid-stream is
  already invisible to today's processor; no new behaviour.
* **Trade identity for consumers.** A `TradeEvent` is identified by `(block_hash, tx_hash,
  log_index)`; the Proposed and Finalized items of one block carry identical trades, a superseding
  item replaces by `(height)`. Documented on `trade`; `types::Trade` itself is unchanged.
* **Tests** (pure, scripted through `TradeProcessor`): same block Proposed then Finalized ⇒
  identical trades, journal promoted; Proposed `h` with `OrderPlaced` id 7, superseding `h'`
  without it, later fill of id 7 ⇒ attribution from the pre-`h` entry; rewind across two
  speculative heights; legacy Voted-only input ⇒ journal stays empty; context does not leak across
  blocks.

## 4. Backward compatibility and migration (requirement 5)

| Existing API | Change |
|---|---|
| `stream::raw(chain, provider, from, sleep)` | unchanged; output tagged `Voted` with the block hash (visible only through new accessors). It may later be reimplemented as `once(EventSource{min: Voted})`, same signature. |
| `stream::trade(...)` | signature and item type unchanged; output now carries the input's tags; `TradeProcessor` gains the undo journal (§3b) so it accepts multi-level streams directly. Legacy `raw` input: identical output. |
| `Exchange::apply_events` | unchanged (commits). |
| `BlockEvents::new` | unchanged; new fields defaulted. New `map_events` helper preserves metadata. |
| `SnapshotBuilder` | unchanged default (`safe`). |
| `testing::Indexer` | unchanged. |
| Cargo | New optional SDK feature `ws` = `alloy/pubsub` + `alloy/provider-ws-ring` + `serde` + `serde_json` + `tokio`, enabling `with_ws_url`; the CLI enables it. The default build pulls no pubsub code. `ring` rather than `aws-lc-rs` on purpose: alloy installs its websocket TLS backend as the process-wide rustls provider, and aws-lc-rs's post-quantum key share made the public testnet HTTPS endpoint reset the handshake, taking the HTTP provider down with it. `DexError` gains two variants, hence the minor bump to 0.3.0. |

Migration ladder: (1) `stream::once(EventSource::new(chain, provider, from).build(sleep).0)`,
same provider as `raw`, same semantics, range catch-up; (2) add `.with_ws_url(..)`, same
semantics, ~300 ms earlier; (3) `.min_commit_state(Proposed)` with
`apply_events_at(&b, Finalized)` for speculative state events and `exchange.speculative()`;
(4) `apply_events_at(&b, Proposed)` to commit speculatively (not recommended; forks surface as
`CommittedBlockMismatch` requiring a rebuild).

CLI (global flags): `--ws-url <URL>` (repeatable; defaults to `wss://rpc.monad.xyz` or
`wss://testnet-rpc.monad.xyz` when the default RPC is used, none with a custom `--rpc`), `--no-ws`,
`--commit-state proposed|voted|finalized` (default `voted`). `trace` consumes the full multi-level
stream, prints every batch with its `[state]` and `[supersedes]` tag, commits at
`max(min, Voted)` through `apply_events_at` and notes re-sends of committed blocks in one line.
The `show` commands consume `stream::once` and apply with `apply_events` as before, so a
`proposed` view ends with `BlockSuperseded` if a shown block is abandoned. The snapshot is taken at
the commit state each command commits at.

## 5. Testing

* Sequencer unit tests (`sequencer.rs`, scripted, no network): polling-only catch-up in ranges
  then tips following, with header-only confirmation; range clamping on "too large"; `Marker`
  sealing with Voted/Finalized re-sends; `NextHeader` sealing including the R2 own-header case;
  bloom-negative header; competing proposal ⇒ superseding emission, parent check filtering the
  stale descendant, Finalized without a flag when it matches; Finalized differing from the
  speculative hash ⇒ superseding; lost connection ⇒ incomplete block completed by a poll, and a
  header after reconnect sealing nothing; Voted skipped by the node ⇒ Finalized only; two sources
  ⇒ one emission; watchdog ⇒ one reconnect after six silent periods; `Quiescent` token timers and
  late-log re-emission.
* Adapter tests (`adapters.rs`): `once` delivers the first batch per height and errors on a
  supersede; `finalized` keeps only finalized batches.
* `Exchange` tests (`src/tests/exchange_overlay.rs`): speculative blocks build the overlay without
  touching committed state (events tagged); committing keeps the speculative blocks above and
  rebuilds the overlay lazily; a superseding proposal rewinds the overlay; committed hash mismatch
  and out-of-order errors in both layers; legacy `apply_events` commits everything and hand-built
  batches never mismatch.
* Trade tests (`trade.rs::journal_tests`): output carries the input tags and the same trades at
  every state; a superseded proposal does not attribute a reused order id; a committed block
  differing from the proposal undoes it; legacy Voted input never journals and transaction scratch
  resets per block.
* Live (`source/mod.rs::tests`, public mainnet endpoint, throttled): polling-only over a window of
  recent blocks, checking the stream contract (runs with `cargo test`); the websocket subscription
  with `min = Proposed` against `wss://rpc.monad.xyz`, checking the contract, that Proposed batches
  and `blockId`s arrive and the connection status (`#[ignore]`, run with `-- --ignored`).
* Anvil (Monad fork 1.8.3) rejects `monadLogs`/`monadNewHeads`, so the existing anvil-based
  integration tests cover the polling path only.

## 6. Questions raised by the first draft, resolved

1. Header vs logs ordering: **header first, strictly, per connection** (code + 990 events on the
   wire). Earlier "out of order" readings were my probe reordering after alloy's dispatch.
2. `eth_getLogs` on a Proposed height: served from the block buffer; consistent with
   `eth_getBlockByNumber` as long as the hash is verified per log (the buffer can be replaced in
   place; the `blockHash` filter does not verify). Design verifies.
3. `blockHash` in `monadLogs` = execution hash, same as `eth_getBlockByNumber`; `blockId` is the
   consensus id. Identity = `(height, hash)`, `blockId` recorded.
4. Range limits: node 1001 blocks, both public RPCs 101 (HTTP and ws). Default chunk 100,
   auto-clamped on error.
5. Anvil: no Monad subscriptions.
6. alloy reconnect: silent, no in-stream signal; lag hidden by stream adapters; stream ends after
   retries. Handled by raw receivers, continuity checks, tip polling, and SDK-managed connections.
7. `DexError` variants: accepted, minor bump.

Still open / to watch:
* Whether two *different* proposals at one height can share an execution hash (theoretically
  relevant to identity; practically irrelevant since identical content yields identical state).
  `blockId` is recorded so this can be detected and the key switched if ever observed.
* ~~Public websocket endpoints~~: resolved, `wss://rpc.monad.xyz` and
  `wss://rpc-mainnet.monadinfra.com` both work (see section 0). Both sit behind Cloudflare, so a
  reconnect may land on a different backend node; the sequencer's `(height, hash)` identity and
  tip polling already cover that, and `blockId` is consensus-level so it is node-independent.
* `Marker` adds a second Proposed-only `logs` subscription (+25% frames). Acceptable; `NextHeader`
  is the no-extra-traffic alternative.

## 7. Implementation map

| Design | Code |
|---|---|
| §1.1 `CommitState`, §1.2 `BlockEvents` tags | `crates/sdk/src/types/event.rs` |
| §1.3 builder, driver, `SourceHandle`/`SourceStatus` | `crates/sdk/src/stream/source/mod.rs` |
| §1.4 adapters | `crates/sdk/src/stream/source/adapters.rs` |
| §2 sequencer (pure) | `crates/sdk/src/stream/source/sequencer.rs` |
| §2.6 polling primitives (tips, hash-verified block, range, split) | `crates/sdk/src/stream/source/poll.rs` |
| §2.4, §2.7 websocket reader, `NoReconnect` connector, logs-first drain | `crates/sdk/src/stream/source/ws.rs` (feature `ws`) |
| §3 overlay | `crates/sdk/src/state/exchange.rs` (`apply_events_at`, `commit`, `apply_block`), `state/mod.rs` (`at_commit_state`) |
| §3b trade journal | `crates/sdk/src/stream/trade.rs` (`reconcile_journal`) |
| §4 errors | `crates/sdk/src/error.rs` (`BlockSuperseded`, `CommittedBlockMismatch`) |
| §4 CLI | `crates/cli/src/args.rs` (flags), `lib.rs` (`Streaming`), `trace.rs`, `book.rs`, `account.rs`, `mms.rs`, `trades.rs` |

`stream::raw` is kept as is (now tagging its output `Voted` with the block hash); reimplementing
it over `EventSource` remains an option.
