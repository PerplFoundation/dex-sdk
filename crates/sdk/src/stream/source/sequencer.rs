//! Pure, synchronous core of [`super::EventSource`]: takes subscription
//! notifications, poll results and timer ticks as [`Input`]s, decides what to
//! emit ([`Output`]) and what to fetch or schedule next ([`Action`]). No I/O,
//! so every scenario is a scripted unit test.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use alloy::primitives::B256;

use super::{
    SealPolicy,
    poll::{BlockHeader, PolledBlock, Tips},
};
use crate::{
    error::DexError,
    stream::{RawBlockEvents, RawEvent},
    types::{CommitState, StateInstant},
};

/// Heights above the next finalized one whose subscription data is kept.
/// Beyond it (catching up from far behind) the polls fetch the blocks once
/// they are due, so memory stays bounded by the lookahead.
const LOOKAHEAD: u64 = 64;

/// Index of a websocket source in the order the URLs were given.
pub(crate) type SourceId = usize;

/// Subscription notification, already decoded.
#[derive(Clone, Debug)]
pub(crate) enum Ws {
    /// `monadNewHeads` notification.
    Header {
        src: SourceId,
        height: u64,
        hash: B256,
        parent: B256,
        id: B256,
        state: CommitState,
        timestamp: u64,
        /// Whether the header's logs bloom may contain the exchange address.
        may_have_logs: bool,
    },
    /// `monadLogs` notification.
    Log {
        src: SourceId,
        height: u64,
        hash: B256,
        id: B256,
        state: CommitState,
        event: Box<RawEvent>,
    },
    /// First standard `logs` frame of a (Proposed) block, written by the node
    /// after the whole `monadLogs` run.
    Marker { src: SourceId, height: u64, hash: B256 },
    /// The connection is gone; a reconnect follows.
    Closed { src: SourceId },
    /// The connection was rebuilt and resubscribed.
    Reconnected { src: SourceId },
    /// A notification could not be decoded.
    Undecodable { src: SourceId },
}

/// Poll result.
#[derive(Debug)]
pub(crate) enum Poll {
    Tips(Tips),
    /// One block, from a per-block or a range fetch.
    Block(PolledBlock),
    /// Header only, to confirm a hash.
    Header(BlockHeader),
    /// The node does not have the block yet.
    Unavailable(PollRequest),
    /// The node refused the range span.
    RangeTooLarge {
        from: u64,
        to: u64,
    },
    /// All blocks of the request were delivered.
    Done(PollRequest),
    /// The request failed; retried next tick unless fatal.
    Failed {
        request: PollRequest,
        error: DexError,
        fatal: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Tick {
    Tip,
    Watchdog,
    /// `Quiescent` sealing timer.
    Quiet {
        src: SourceId,
        height: u64,
        hash: B256,
        token: u64,
    },
}

#[derive(Debug)]
pub(crate) enum Input {
    Ws(Ws),
    Poll(Poll),
    Tick(Tick),
}

/// What the driver should fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PollRequest {
    /// The `finalized` tip and, when polling drives the speculative layer,
    /// the tip of the given state.
    Tips(Option<CommitState>),
    Block(u64),
    Header(u64),
    Range {
        from: u64,
        to: u64,
    },
}

/// What the driver should do.
#[derive(Debug)]
pub(crate) enum Action {
    Poll(PollRequest),
    Timer(Duration, Tick),
    /// Tear the connection down and rebuild it (watchdog verdict).
    Reconnect(SourceId),
}

#[derive(Debug)]
pub(crate) enum Output {
    Emit(RawBlockEvents),
    Error(DexError),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    pub min: CommitState,
    pub seal: SealPolicy,
    pub poll_chunk: u64,
    pub poll_concurrency: usize,
    /// Tips period while a subscription is alive (polling is a safety net).
    pub tip_interval: Duration,
    /// Tips period while no subscription is alive (polling drives the
    /// stream).
    pub poll_interval: Duration,
    pub watchdog: Duration,
    /// Watchdog periods without a header before a source is rebuilt.
    pub stale_periods: u32,
    pub sources: usize,
}

#[derive(Debug)]
struct Candidate {
    /// Order of first sight, to break ties between proposals.
    seq: u64,
    id: Option<B256>,
    state: CommitState,
    header: Option<Header>,
    logs: BTreeMap<u64, RawEvent>,
    /// All logs are in: sealed by a subscription (§sealing rules in
    /// `on_ws`) or fetched by a poll.
    logs_complete: bool,
    /// Bumped on every log; `Quiescent` timers carry the value they saw.
    log_token: u64,
    /// Emitted speculatively and then changed (late log under `Quiescent`).
    dirty: bool,
    /// The polling provider vouches for `Finalized`: by a tip naming the
    /// hash, or by a fetch issued once the height was finalized.
    confirmed: bool,
    /// Header and logs were fetched from the polling provider.
    polled: bool,
}

#[derive(Clone, Copy, Debug)]
struct Header {
    timestamp: u64,
    parent: Option<B256>,
}

impl Candidate {
    /// Emittable: logs sealed and header known.
    fn complete(&self) -> bool { self.logs_complete && self.header.is_some() }
}

/// Identity of one `monadLogs` run: the node streams the logs of one block
/// at one commit state contiguously.
type RunKey = (u64, B256, CommitState);

#[derive(Debug, Default)]
struct Source {
    /// Run currently streaming on this connection's `monadLogs` channel.
    run: Option<RunKey>,
    /// Last header on this connection: a run starts right after its header.
    header: Option<RunKey>,
    /// A log arrived since `header`.
    logged: bool,
    headers_seen: u64,
    headers_at_last_tick: u64,
    stale: u32,
    alive: bool,
}

/// Live view of the sequencer, see [`super::SourceHandle::status`].
#[derive(Clone, Debug, Default)]
pub struct Cursors {
    /// Next height to be emitted as `Finalized`.
    pub next_finalized: u64,
    /// Next height to be emitted speculatively.
    pub next_speculative: u64,
    /// Last tips fetched, if any.
    pub tips: Option<Tips>,
    /// Current `eth_getLogs` span, after clamping.
    pub poll_chunk: u64,
}

#[derive(Debug)]
pub(crate) struct Sequencer {
    opts: Options,
    next_final: u64,
    /// Hash of the last finalized block emitted, the parent every
    /// speculative chain must build on.
    final_hash: Option<B256>,
    spec_next: u64,
    spec_emitted: BTreeMap<u64, B256>,
    candidates: BTreeMap<u64, HashMap<B256, Candidate>>,
    /// Heights that got a new proposal after being emitted speculatively.
    pending_rewind: HashSet<u64>,
    tips: Option<Tips>,
    /// Requests in flight, each with the tips it was issued under: what a
    /// response proves about the block's commit state.
    in_flight: HashMap<PollRequest, Option<Tips>>,
    chunk: u64,
    sources: Vec<Source>,
    seq: u64,
    want_tips: bool,
    outputs: Vec<Output>,
    actions: Vec<Action>,
    /// Whether anything speculative is wanted at all.
    speculative: bool,
}

impl Sequencer {
    pub fn new(from: u64, opts: Options) -> Self {
        let mut sources = Vec::with_capacity(opts.sources);
        sources.resize_with(opts.sources, Source::default);
        Self {
            speculative: opts.min < CommitState::Finalized,
            chunk: opts.poll_chunk.max(1),
            opts,
            next_final: from,
            final_hash: None,
            spec_next: from,
            spec_emitted: BTreeMap::new(),
            candidates: BTreeMap::new(),
            pending_rewind: HashSet::new(),
            tips: None,
            in_flight: HashMap::new(),
            sources,
            seq: 0,
            want_tips: false,
            outputs: Vec::new(),
            actions: Vec::new(),
        }
    }

    /// Actions to take before any input: first tips check and the timers.
    pub fn start(&mut self) -> Vec<Action> {
        self.request_tips();
        self.actions
            .push(Action::Timer(self.tips_period(), Tick::Tip));
        if !self.sources.is_empty() {
            self.actions
                .push(Action::Timer(self.opts.watchdog, Tick::Watchdog));
        }
        std::mem::take(&mut self.actions)
    }

    pub fn cursors(&self) -> Cursors {
        Cursors {
            next_finalized: self.next_final,
            next_speculative: self.spec_next,
            tips: self.tips,
            poll_chunk: self.chunk,
        }
    }

    /// Feeds one input; returns what to emit and what to do next, in order.
    pub fn push(&mut self, input: Input) -> (Vec<Output>, Vec<Action>) {
        match input {
            Input::Ws(ws) => self.on_ws(ws),
            Input::Poll(poll) => self.on_poll(poll),
            Input::Tick(tick) => self.on_tick(tick),
        }
        self.drain();
        if self.want_tips {
            self.want_tips = false;
            self.request_tips();
        }
        (std::mem::take(&mut self.outputs), std::mem::take(&mut self.actions))
    }

    /// No subscription is alive (none configured, none connected yet, or all
    /// dead): polling drives the stream rather than backing it.
    fn polling_drives(&self) -> bool { !self.sources.iter().any(|s| s.alive) }

    fn tips_period(&self) -> Duration {
        if self.polling_drives() { self.opts.poll_interval } else { self.opts.tip_interval }
    }

    /// Requests the tips unless a request is in flight. The speculative tip
    /// is only fetched while polling drives the stream; otherwise the
    /// subscriptions deliver speculative blocks and only finality is polled.
    fn request_tips(&mut self) {
        if self
            .in_flight
            .keys()
            .any(|r| matches!(r, PollRequest::Tips(_)))
        {
            return;
        }
        let speculative = (self.polling_drives() && self.speculative).then_some(self.opts.min);
        self.request(PollRequest::Tips(speculative));
    }

    fn request(&mut self, req: PollRequest) {
        if let std::collections::hash_map::Entry::Vacant(e) = self.in_flight.entry(req) {
            e.insert(self.tips);
            self.actions.push(Action::Poll(req));
        }
    }

    fn candidate(&mut self, height: u64, hash: B256) -> &mut Candidate {
        let seq = &mut self.seq;
        let emitted = self.spec_emitted.get(&height).copied();
        let pending = &mut self.pending_rewind;
        self.candidates
            .entry(height)
            .or_default()
            .entry(hash)
            .or_insert_with(|| {
                *seq += 1;
                if emitted.is_some_and(|h| h != hash) {
                    pending.insert(height);
                }
                Candidate {
                    seq: *seq,
                    id: None,
                    state: CommitState::Proposed,
                    header: None,
                    logs: BTreeMap::new(),
                    logs_complete: false,
                    log_token: 0,
                    dirty: false,
                    confirmed: false,
                    polled: false,
                }
            })
    }

    /// Sealing rules. The websocket reader delivers frames in wire order and
    /// the node writes one event (header, then its logs, then the standard
    /// `logs`) to completion before the next, so:
    ///
    /// * R1: a log of another run ends the current run.
    /// * R2: a header ends the current run (a repeated header of the run itself
    ///   does not).
    /// * R3: a Proposed marker ends the run of its block, provided the run was
    ///   seen at all.
    /// * R4: a header whose bloom excludes the exchange needs no logs.
    /// * R5: a lost connection ends nothing; polling completes the block.
    /// * R6: `Quiescent` ends the current run after a quiet period.
    fn on_ws(&mut self, ws: Ws) {
        match ws {
            Ws::Header { src, height, hash, parent, id, state, timestamp, may_have_logs } => {
                let key = (height, hash, state.normalized());
                if self.sources[src].run.is_some_and(|run| run != key) {
                    self.close_run(src); // R2
                }
                // R2 for a block without logs: header followed by header
                let source = &mut self.sources[src];
                if let Some((h, hs, _)) = source.header
                    && source.header != Some(key)
                    && !source.logged
                    && let Some(cand) = self.candidates.get_mut(&h).and_then(|c| c.get_mut(&hs))
                    && cand.logs.is_empty()
                {
                    cand.logs_complete = true;
                }
                let source = &mut self.sources[src];
                if source.header != Some(key) {
                    source.logged = false;
                }
                source.header = Some(key);
                source.headers_seen += 1;
                source.alive = true;
                if !self.buffers(height) {
                    return;
                }
                let seal = self.opts.seal;
                let cand = self.candidate(height, hash);
                cand.id = Some(id);
                cand.state = cand.state.max(state.normalized());
                cand.header = Some(Header { timestamp, parent: Some(parent) });
                if !may_have_logs {
                    cand.logs_complete = true; // R4
                }
                let token = cand.log_token;
                let complete = cand.logs_complete;
                if !complete && let SealPolicy::Quiescent { quiet } = seal {
                    self.actions
                        .push(Action::Timer(quiet, Tick::Quiet { src, height, hash, token }));
                }
            },
            Ws::Log { src, height, hash, id, state, event } => {
                let key = (height, hash, state.normalized());
                if self.sources[src].run.is_some_and(|run| run != key) {
                    self.close_run(src); // R1
                }
                // A run joined midway (its header went out before the
                // connection was up) is not one: nothing seals it.
                let source = &mut self.sources[src];
                source.logged = true;
                if source.run.is_none() && source.header == Some(key) {
                    source.run = Some(key);
                }
                if !self.buffers(height) {
                    return;
                }
                let seal = self.opts.seal;
                let cand = self.candidate(height, hash);
                cand.id.get_or_insert(id);
                cand.state = cand.state.max(state.normalized());
                cand.log_token += 1;
                let index = event.log_index();
                let fresh = cand.logs.insert(index, *event).is_none();
                if fresh && cand.logs_complete {
                    // A log after the block was sealed: a `Quiescent` timer
                    // that fired early, or a later-state run carrying more
                    // than the Proposed one did (never observed).
                    cand.logs_complete = false;
                    cand.dirty = true;
                }
                let token = cand.log_token;
                if !cand.logs_complete
                    && let SealPolicy::Quiescent { quiet } = seal
                {
                    self.actions
                        .push(Action::Timer(quiet, Tick::Quiet { src, height, hash, token }));
                }
            },
            Ws::Marker { src, height, hash } => {
                // R3: only a run seen from its header on is sealed
                if self.sources[src]
                    .run
                    .is_some_and(|(h, hs, _)| (h, hs) == (height, hash))
                {
                    self.close_run(src);
                }
            },
            Ws::Undecodable { src } => {
                // R5; the reader rebuilds the connection.
                self.abort_run(src);
                self.want_tips = true;
            },
            Ws::Closed { src } => {
                self.abort_run(src);
                self.sources[src].alive = false;
                self.want_tips = true;
            },
            Ws::Reconnected { src } => {
                self.abort_run(src);
                self.sources[src].alive = true;
                self.sources[src].stale = 0;
                self.want_tips = true;
            },
        }
    }

    /// Whether subscription data at `height` is kept.
    fn buffers(&self, height: u64) -> bool {
        (self.next_final..=self.next_final.saturating_add(LOOKAHEAD)).contains(&height)
    }

    /// The run streaming on `src` ended regularly: its block has all its
    /// logs.
    fn close_run(&mut self, src: SourceId) {
        if let Some((height, hash, _)) = self.sources[src].run.take()
            && let Some(cand) = self
                .candidates
                .get_mut(&height)
                .and_then(|c| c.get_mut(&hash))
        {
            cand.logs_complete = true;
        }
    }

    /// The run streaming on `src` was cut: its block stays incomplete unless
    /// another source or a poll completes it. Its pending `Quiescent` timers
    /// are voided, as a cut run is no more quiet than complete.
    fn abort_run(&mut self, src: SourceId) {
        self.sources[src].header = None;
        if let Some((height, hash, _)) = self.sources[src].run.take()
            && let Some(cand) = self
                .candidates
                .get_mut(&height)
                .and_then(|c| c.get_mut(&hash))
        {
            cand.log_token += 1;
        }
    }

    fn on_poll(&mut self, poll: Poll) {
        match poll {
            Poll::Tips(tips) => {
                self.in_flight
                    .retain(|r, _| !matches!(r, PollRequest::Tips(_)));
                self.tips = Some(tips);
                self.confirm_named(&tips);
                self.fill_gaps();
            },
            Poll::Block(block) => {
                let height = block.header.number;
                let issued = self
                    .in_flight
                    .remove(&PollRequest::Block(height))
                    .or_else(|| {
                        self.in_flight.iter().find_map(|(r, tips)| match r {
                            PollRequest::Range { from, to } if (*from..=*to).contains(&height) => {
                                Some(*tips)
                            },
                            _ => None,
                        })
                    });
                if height < self.next_final {
                    return;
                }
                let state = self.polled_state(height, block.header.hash, issued.flatten());
                let cand = self.candidate(height, block.header.hash);
                cand.state = cand.state.max(state);
                cand.confirmed |= state >= CommitState::Finalized;
                cand.polled = true;
                cand.header = Some(Header {
                    timestamp: block.header.timestamp,
                    parent: Some(block.header.parent),
                });
                cand.logs = block
                    .events
                    .into_iter()
                    .map(|e| (e.log_index(), e))
                    .collect();
                cand.logs_complete = true;
            },
            Poll::Header(header) => {
                let issued = self.in_flight.remove(&PollRequest::Header(header.number));
                if header.number < self.next_final {
                    return;
                }
                let state = self.polled_state(header.number, header.hash, issued.flatten());
                let cand = self.candidate(header.number, header.hash);
                cand.state = cand.state.max(state);
                cand.confirmed |= state >= CommitState::Finalized;
                cand.header.get_or_insert(Header {
                    timestamp: header.timestamp,
                    parent: Some(header.parent),
                });
            },
            Poll::Unavailable(req) => {
                self.in_flight.remove(&req);
            },
            Poll::RangeTooLarge { from, to } => {
                self.in_flight.remove(&PollRequest::Range { from, to });
                let span = to - from + 1;
                self.chunk = (span / 2).max(1).min(self.chunk);
                self.fill_gaps();
            },
            Poll::Done(req) => {
                self.in_flight.remove(&req);
                self.fill_gaps();
            },
            Poll::Failed { request, error, fatal } => {
                self.in_flight.remove(&request);
                if fatal {
                    self.outputs.push(Output::Error(error));
                } else {
                    self.want_tips = true;
                }
            },
        }
    }

    /// Commit state a polled block is known to have: what the tips said
    /// about its height when the request went out (a block fetched while
    /// only proposed may be a proposal abandoned since), or what the current
    /// tips say about its hash.
    fn polled_state(&self, height: u64, hash: B256, issued: Option<Tips>) -> CommitState {
        let by_height = issued.map_or(CommitState::Proposed, |t| t.state_of(height));
        let Some(tips) = self.tips else { return by_height };
        tips.named()
            .filter_map(|(h, hs, state)| (h == height && hs == hash).then_some(state))
            .fold(by_height, CommitState::max)
    }

    fn on_tick(&mut self, tick: Tick) {
        match tick {
            Tick::Tip => {
                self.request_tips();
                self.actions
                    .push(Action::Timer(self.tips_period(), Tick::Tip));
            },
            Tick::Watchdog => {
                let mut any_stale = false;
                for (src, source) in self.sources.iter_mut().enumerate() {
                    if !source.alive {
                        continue;
                    }
                    if source.headers_seen == source.headers_at_last_tick {
                        source.stale += 1;
                        any_stale = true;
                        if source.stale >= self.opts.stale_periods {
                            source.stale = 0;
                            source.alive = false;
                            self.actions.push(Action::Reconnect(src));
                        }
                    } else {
                        source.stale = 0;
                    }
                    source.headers_at_last_tick = source.headers_seen;
                }
                let rebuilt: Vec<SourceId> = self
                    .actions
                    .iter()
                    .filter_map(|a| match a {
                        Action::Reconnect(src) => Some(*src),
                        _ => None,
                    })
                    .collect();
                for src in rebuilt {
                    self.abort_run(src);
                }
                if any_stale {
                    self.want_tips = true;
                }
                self.actions
                    .push(Action::Timer(self.opts.watchdog, Tick::Watchdog));
            },
            Tick::Quiet { src, height, hash, token } => {
                // R6
                let streaming = self.sources[src]
                    .run
                    .is_none_or(|(h, hs, _)| (h, hs) == (height, hash));
                if streaming
                    && let Some(cand) = self
                        .candidates
                        .get_mut(&height)
                        .and_then(|c| c.get_mut(&hash))
                    && cand.log_token == token
                    && cand.header.is_some()
                {
                    cand.logs_complete = true;
                }
            },
        }
    }

    /// A block the tips name by hash has reached the tip's state: a
    /// candidate with that hash is promoted without a confirming fetch. This
    /// is what lets a prefetched proposed block go out as `Voted` the moment
    /// the `safe` tip reaches it, and a complete candidate go out as
    /// `Finalized` without the header round trip.
    fn confirm_named(&mut self, tips: &Tips) {
        for (height, hash, state) in tips.named() {
            if height < self.next_final {
                continue;
            }
            if let Some(cand) = self
                .candidates
                .get_mut(&height)
                .and_then(|c| c.get_mut(&hash))
            {
                cand.state = cand.state.max(state);
                cand.confirmed |= state >= CommitState::Finalized;
            }
        }
    }

    /// Requests whatever polling is needed to make progress, given the tips.
    fn fill_gaps(&mut self) {
        let Some(tips) = self.tips else { return };

        // Finalized layer: range-poll everything finalized that is not yet
        // complete, one chunk at a time.
        if tips.finalized.number >= self.next_final {
            let needs_poll = !self.has_complete_at(self.next_final, CommitState::Finalized);
            let range_in_flight = self
                .in_flight
                .keys()
                .any(|r| matches!(r, PollRequest::Range { .. }));
            if needs_poll && !range_in_flight {
                let to = tips
                    .finalized
                    .number
                    .min(self.next_final.saturating_add(self.chunk - 1));
                // A single missing finalized block right behind the tip is
                // the common case after a short outage; fetch it directly.
                if to == self.next_final {
                    self.request(PollRequest::Block(to));
                } else {
                    self.request(PollRequest::Range { from: self.next_final, to });
                }
            }
        }

        // Speculative layer: per-block polls for heights the subscriptions
        // have not delivered, bounded by the poll concurrency. While polling
        // drives, the tips reach `latest` whatever the minimum state, so
        // proposed blocks are prefetched and held until the `safe` tip names
        // them; a height is known only if the proposal the tips name (when
        // they name one) is in hand, so a superseded prefetch is refetched.
        if self.speculative {
            let top = tips.latest.number;
            let mut budget = self.opts.poll_concurrency;
            // Heights up to the finalized tip belong to the range polls.
            let mut h = self
                .spec_next
                .max(self.next_final)
                .max(tips.finalized.number + 1);
            while h <= top && budget > 0 {
                let fresh = |cand: &Candidate| cand.complete() || cand.header.is_some();
                let known = self
                    .candidates
                    .get(&h)
                    .is_some_and(|c| match tips.hash_at(h) {
                        Some(hash) => c.get(&hash).is_some_and(fresh),
                        None => c.values().any(fresh),
                    });
                if !known {
                    self.request(PollRequest::Block(h));
                    budget -= 1;
                }
                h += 1;
            }
        }
    }

    fn has_complete_at(&self, height: u64, min: CommitState) -> bool {
        self.candidates
            .get(&height)
            .is_some_and(|c| c.values().any(|cand| cand.complete() && cand.state >= min))
    }

    /// Emits everything that became emittable.
    fn drain(&mut self) {
        if self.speculative {
            self.drain_rewinds();
            self.drain_speculative();
        }
        self.drain_finalized();
        if self.speculative {
            // Finalizing may have unblocked speculative heights above.
            self.drain_speculative();
        }
    }

    fn drain_rewinds(&mut self) {
        let mut heights: Vec<u64> = self.pending_rewind.iter().copied().collect();
        heights.sort_unstable();
        for h in heights {
            if h < self.next_final {
                self.pending_rewind.remove(&h);
                continue;
            }
            let Some(emitted) = self.spec_emitted.get(&h).copied() else {
                self.pending_rewind.remove(&h);
                continue;
            };
            let Some(pick) = self.pick_speculative(h, Some(emitted)) else { continue };
            self.pending_rewind.remove(&h);
            self.emit_speculative(h, pick, true);
        }
        // Late logs under `Quiescent`: same hash, re-emitted as superseding.
        let dirty: Vec<(u64, B256)> = self
            .spec_emitted
            .iter()
            .filter(|(h, hash)| {
                self.candidates
                    .get(*h)
                    .and_then(|c| c.get(*hash))
                    .is_some_and(|c| c.dirty && c.complete())
            })
            .map(|(h, hash)| (*h, *hash))
            .collect();
        for (h, hash) in dirty {
            self.emit_speculative(h, hash, true);
        }
    }

    fn drain_speculative(&mut self) {
        loop {
            if self.spec_next < self.next_final {
                self.spec_next = self.next_final;
            }
            let h = self.spec_next;
            let Some(hash) = self.pick_speculative(h, None) else { break };
            self.emit_speculative(h, hash, false);
        }
    }

    /// Best complete candidate at `h` for the speculative layer: highest
    /// state below Finalized, at or above `min`, most recent on ties, with a
    /// parent consistent with what was emitted at `h - 1`. `not` excludes the
    /// hash already emitted at `h`.
    fn pick_speculative(&self, h: u64, not: Option<B256>) -> Option<B256> {
        let parent_hash = if h == self.next_final {
            self.final_hash
        } else {
            self.spec_emitted.get(&(h.wrapping_sub(1))).copied()
        };
        let cands = self.candidates.get(&h)?;
        cands
            .iter()
            .filter(|(hash, c)| {
                c.complete()
                    && c.state >= self.opts.min
                    && c.state < CommitState::Finalized
                    && not.is_none_or(|n| n != **hash)
                    && match (parent_hash, c.header.and_then(|hd| hd.parent)) {
                        (Some(p), Some(cp)) => p == cp,
                        _ => true,
                    }
            })
            .max_by_key(|(_, c)| (c.state, c.seq))
            .map(|(hash, _)| *hash)
    }

    fn emit_speculative(&mut self, h: u64, hash: B256, supersedes: bool) {
        let Some(cand) = self.candidates.get_mut(&h).and_then(|c| c.get_mut(&hash)) else {
            return;
        };
        cand.dirty = false;
        let events = Self::to_events(h, hash, cand, cand.state, supersedes);
        if supersedes {
            self.spec_emitted.retain(|k, _| *k < h);
        }
        self.spec_emitted.insert(h, hash);
        self.spec_next = h + 1;
        self.outputs.push(Output::Emit(events));
    }

    fn drain_finalized(&mut self) {
        loop {
            let h = self.next_final;
            let Some(cands) = self.candidates.get(&h) else {
                self.poll_finalized_if_due(h);
                break;
            };
            // Only a block building on the last finalized one is emitted; the
            // polling provider's word beats a subscription's, a complete
            // candidate an incomplete one
            let builds_on_final = |c: &Candidate| match (self.final_hash, c.header) {
                (Some(p), Some(Header { parent: Some(cp), .. })) => p == cp,
                _ => true,
            };
            let finalized = cands
                .iter()
                .filter(|(_, c)| c.state >= CommitState::Finalized)
                .max_by_key(|(_, c)| (builds_on_final(c), c.confirmed, c.complete(), c.seq))
                .map(|(hash, c)| {
                    (*hash, builds_on_final(c) && c.complete(), builds_on_final(c), c.polled)
                });
            match finalized {
                Some((_, false, false, true)) => {
                    // The polling provider's own finalized block does not
                    // build on the one emitted below it
                    self.outputs
                        .push(Output::Error(DexError::BlockSuperseded(h.saturating_sub(1))));
                    break;
                },
                Some((hash, true, _, _)) => {
                    let cand = self
                        .candidates
                        .get_mut(&h)
                        .and_then(|c| c.get_mut(&hash))
                        .unwrap();
                    let supersedes = self.spec_emitted.get(&h).is_some_and(|x| *x != hash);
                    let events = Self::to_events(h, hash, cand, CommitState::Finalized, supersedes);
                    self.outputs.push(Output::Emit(events));
                    self.candidates = self.candidates.split_off(&(h + 1));
                    self.spec_emitted = self.spec_emitted.split_off(&(h + 1));
                    self.pending_rewind.retain(|k| *k > h);
                    self.next_final = h + 1;
                    self.final_hash = Some(hash);
                    if supersedes {
                        // Everything speculative above was built on the
                        // abandoned proposal.
                        self.spec_emitted.clear();
                        self.spec_next = h + 1;
                    } else if self.spec_next <= h {
                        self.spec_next = h + 1;
                    }
                },
                Some(_) => {
                    self.request(PollRequest::Block(h));
                    break;
                },
                None => {
                    // Known only speculatively: once the tips say it is
                    // finalized, confirm the hash with a header fetch.
                    if self.tips.is_some_and(|t| t.finalized.number >= h) {
                        let complete = cands.values().any(|c| c.complete());
                        if complete {
                            self.request(PollRequest::Header(h));
                        } else {
                            self.request(PollRequest::Block(h));
                        }
                    }
                    break;
                },
            }
        }
    }

    fn poll_finalized_if_due(&mut self, h: u64) {
        if self.tips.is_some_and(|t| t.finalized.number >= h)
            && !self
                .in_flight
                .keys()
                .any(|r| matches!(r, PollRequest::Range { .. } | PollRequest::Block(_)))
        {
            self.fill_gaps();
        }
    }

    fn to_events(
        h: u64,
        hash: B256,
        cand: &Candidate,
        state: CommitState,
        supersedes: bool,
    ) -> RawBlockEvents {
        let timestamp = cand.header.map(|hd| hd.timestamp).unwrap_or_default();
        let mut events = RawBlockEvents::new(
            StateInstant::new(h, timestamp),
            cand.logs.values().cloned().collect(),
        )
        .with_commit_state(state)
        .with_block_hash(hash)
        .superseding(supersedes);
        if let Some(id) = cand.id {
            events = events.with_block_id(id);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{TxHash, U256, b256};

    use super::*;
    use crate::{
        abi::dex::Exchange::{AccountCreated, ExchangeEvents},
        stream::source::poll::Tip,
    };

    const A: B256 = b256!("0x00000000000000000000000000000000000000000000000000000000000000aa");
    const B: B256 = b256!("0x00000000000000000000000000000000000000000000000000000000000000bb");
    const C: B256 = b256!("0x00000000000000000000000000000000000000000000000000000000000000cc");

    fn hash(h: u64) -> B256 { B256::from(U256::from(h)) }

    /// Parent of a polled test block: the sequential hash of the height
    /// below, or the fixed one when that height was given a letter hash.
    fn hash_of(h: u64) -> B256 { hash(h) }

    fn opts(min: CommitState, seal: SealPolicy, sources: usize) -> Options {
        Options {
            min,
            seal,
            poll_chunk: 100,
            poll_concurrency: 4,
            tip_interval: Duration::from_millis(500),
            poll_interval: Duration::from_millis(100),
            watchdog: Duration::from_millis(500),
            stale_periods: 2,
            sources,
        }
    }

    /// The tips request among the actions, if any, with its speculative tag.
    fn tips_request(actions: &[Action]) -> Option<Option<CommitState>> {
        actions.iter().find_map(|a| match a {
            Action::Poll(PollRequest::Tips(s)) => Some(*s),
            _ => None,
        })
    }

    /// The period of the tips timer among the actions.
    fn tips_timer(actions: &[Action]) -> Option<Duration> {
        actions.iter().find_map(|a| match a {
            Action::Timer(d, Tick::Tip) => Some(*d),
            _ => None,
        })
    }

    fn event(index: u64) -> RawEvent {
        RawEvent::new(
            TxHash::ZERO,
            0,
            index,
            ExchangeEvents::AccountCreated(AccountCreated {
                id: U256::from(index),
                account: Default::default(),
            }),
        )
    }

    fn header(src: SourceId, height: u64, hash: B256, parent: B256, state: CommitState) -> Input {
        Input::Ws(Ws::Header {
            src,
            height,
            hash,
            parent,
            id: hash,
            state,
            timestamp: height * 10,
            may_have_logs: true,
        })
    }

    fn log(src: SourceId, height: u64, hash: B256, state: CommitState, index: u64) -> Input {
        Input::Ws(Ws::Log { src, height, hash, id: hash, state, event: Box::new(event(index)) })
    }

    fn marker(src: SourceId, height: u64, hash: B256) -> Input {
        Input::Ws(Ws::Marker { src, height, hash })
    }

    /// Tips by height only, naming no blocks.
    fn tips(latest: u64, safe: u64, finalized: u64) -> Input {
        Input::Poll(Poll::Tips(Tips {
            latest: Tip::at(latest),
            safe: Tip::at(safe),
            finalized: Tip::at(finalized),
        }))
    }

    /// A tip naming its block by the test hash of its height.
    fn tip(number: u64) -> Tip {
        Tip { number, hash: Some(hash(number)), parent: Some(hash(number - 1)) }
    }

    /// Tips naming their blocks by the test hashes of their heights.
    fn named_tips(latest: u64, safe: u64, finalized: u64) -> Input {
        Input::Poll(Poll::Tips(Tips {
            latest: tip(latest),
            safe: tip(safe),
            finalized: tip(finalized),
        }))
    }

    fn polled(height: u64, hash: B256, n: usize) -> Input {
        Input::Poll(Poll::Block(PolledBlock {
            header: BlockHeader {
                number: height,
                hash,
                parent: hash_of(height - 1),
                timestamp: height * 10,
            },
            events: (0..n as u64).map(event).collect(),
        }))
    }

    fn emitted(outputs: &[Output]) -> Vec<(u64, CommitState, bool, usize)> {
        outputs
            .iter()
            .map(|o| match o {
                Output::Emit(b) => {
                    (b.block_number(), b.commit_state(), b.supersedes(), b.events().len())
                },
                Output::Error(e) => panic!("unexpected error {e}"),
            })
            .collect()
    }

    fn polls(actions: &[Action]) -> Vec<PollRequest> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Poll(r) => Some(*r),
                _ => None,
            })
            .collect()
    }

    fn feed(seq: &mut Sequencer, inputs: Vec<Input>) -> (Vec<Output>, Vec<Action>) {
        let mut outs = vec![];
        let mut acts = vec![];
        for input in inputs {
            let (o, a) = seq.push(input);
            outs.extend(o);
            acts.extend(a);
        }
        (outs, acts)
    }

    #[test]
    fn polling_only_catches_up_in_ranges_then_follows_tips() {
        let mut seq = Sequencer::new(100, opts(CommitState::Voted, SealPolicy::Marker, 0));
        let start = seq.start();
        assert_eq!(polls(&start), vec![PollRequest::Tips(Some(CommitState::Voted))]);

        // Finalized heights go in one range, the Voted one by itself, and
        // the latest one is prefetched.
        let (outs, acts) = seq.push(tips(107, 106, 105));
        assert!(outs.is_empty());
        assert_eq!(
            polls(&acts),
            vec![
                PollRequest::Range { from: 100, to: 105 },
                PollRequest::Block(106),
                PollRequest::Block(107)
            ]
        );

        // The Voted block arriving first waits for the finalized ones; the
        // prefetched Proposed one is held below the minimum state.
        let (outs, _) = seq.push(polled(107, hash(107), 1));
        assert!(outs.is_empty());
        let (outs, _) = seq.push(polled(106, hash(106), 2));
        assert!(outs.is_empty());
        let blocks = (100..=105).map(|h| polled(h, hash(h), 1)).collect();
        let (outs, acts) = feed(&mut seq, blocks);
        let mut expected: Vec<_> = (100..=105)
            .map(|h| (h, CommitState::Finalized, false, 1))
            .collect();
        expected.push((106, CommitState::Voted, false, 2));
        assert_eq!(emitted(&outs), expected);
        assert!(polls(&acts).is_empty(), "range still in flight: {acts:?}");
        let (outs, acts) =
            seq.push(Input::Poll(Poll::Done(PollRequest::Range { from: 100, to: 105 })));
        assert!(outs.is_empty());
        assert!(polls(&acts).is_empty(), "{acts:?}");

        // Tips move but name no hashes (the node had no block to name): the
        // prefetched 107 stays held and the finalized layer confirms 106 by
        // header, as nothing vouches for the candidates in hand.
        let (outs, acts) = seq.push(tips(108, 107, 106));
        assert!(outs.is_empty());
        assert!(polls(&acts).contains(&PollRequest::Header(106)), "{acts:?}");
        let (outs, _) = seq.push(Input::Poll(Poll::Header(BlockHeader {
            number: 106,
            hash: hash(106),
            parent: B256::ZERO,
            timestamp: 1060,
        })));
        assert_eq!(emitted(&outs), vec![(106, CommitState::Finalized, false, 2)]);
        assert_eq!(seq.cursors().next_finalized, 107);
    }

    #[test]
    fn named_tips_promote_candidates_without_a_confirming_fetch() {
        let mut seq = Sequencer::new(106, opts(CommitState::Voted, SealPolicy::Marker, 0));
        seq.start();
        let (_, acts) = seq.push(named_tips(107, 106, 105));
        assert_eq!(polls(&acts), vec![PollRequest::Block(106), PollRequest::Block(107)]);
        let (outs, _) = seq.push(polled(107, hash(107), 1));
        assert!(outs.is_empty(), "prefetched Proposed block is held");
        let (outs, _) = seq.push(polled(106, hash(106), 2));
        assert_eq!(emitted(&outs), vec![(106, CommitState::Voted, false, 2)]);

        // The tips name 106 finalized and 107 safe: both go out at once, with
        // no header fetch for 106, and 108 is prefetched.
        let (outs, acts) = seq.push(named_tips(108, 107, 106));
        assert_eq!(
            emitted(&outs),
            vec![(107, CommitState::Voted, false, 1), (106, CommitState::Finalized, false, 2)]
        );
        assert_eq!(polls(&acts), vec![PollRequest::Block(108)]);
        assert_eq!(seq.cursors().next_finalized, 107);
        let (outs, _) = seq.push(polled(108, hash(108), 1));
        assert!(outs.is_empty());

        // The prefetched proposal at 108 is superseded: the tips name another
        // hash there, so 108 is fetched again and the fresh one goes out
        // once voted.
        let other = hash(1008);
        let superseding = Tips {
            latest: Tip { number: 108, hash: Some(other), parent: Some(hash(107)) },
            safe: tip(107),
            finalized: tip(107),
        };
        let (outs, acts) = seq.push(Input::Poll(Poll::Tips(superseding)));
        assert_eq!(emitted(&outs), vec![(107, CommitState::Finalized, false, 1)]);
        assert_eq!(polls(&acts), vec![PollRequest::Block(108)]);
        let (outs, _) = seq.push(polled(108, other, 3));
        assert!(outs.is_empty());

        // The `safe` tip was missed between two polls, but the next proposal
        // names its parent: 108 goes out as Voted on the strength of 109's
        // parent hash, and 109 is prefetched.
        let voted = Tips {
            latest: Tip { number: 109, hash: Some(hash(109)), parent: Some(other) },
            ..superseding
        };
        let (outs, acts) = seq.push(Input::Poll(Poll::Tips(voted)));
        assert_eq!(emitted(&outs), vec![(108, CommitState::Voted, false, 3)]);
        assert_eq!(polls(&acts), vec![PollRequest::Block(109)]);
        assert_eq!(seq.cursors().next_speculative, 109);
    }

    #[test]
    fn stale_prefetch_is_not_finalized_by_newer_tips() {
        let x = hash(1006);
        let mut seq = Sequencer::new(106, opts(CommitState::Voted, SealPolicy::Marker, 0));
        seq.start();
        // 106 is prefetched while the tips name proposal X there.
        let proposed = Tips {
            latest: Tip { number: 106, hash: Some(x), parent: Some(hash(105)) },
            safe: Tip::at(105),
            finalized: Tip::at(105),
        };
        let (_, acts) = seq.push(Input::Poll(Poll::Tips(proposed)));
        assert_eq!(polls(&acts), vec![PollRequest::Block(106)]);
        // X is replaced by Y = hash(106), which finalizes before the
        // prefetch returns.
        seq.push(named_tips(107, 106, 106));
        let (outs, acts) = seq.push(polled(106, x, 1));
        assert!(outs.is_empty(), "stale prefetch emitted: {:?}", emitted(&outs));
        assert!(
            polls(&acts)
                .iter()
                .any(|r| matches!(r, PollRequest::Block(106) | PollRequest::Header(106))),
            "{acts:?}"
        );
        let (outs, _) = seq.push(polled(106, hash(106), 2));
        assert_eq!(emitted(&outs), vec![(106, CommitState::Finalized, false, 2)]);
    }

    #[test]
    fn range_too_large_clamps_the_chunk() {
        let mut seq = Sequencer::new(100, opts(CommitState::Finalized, SealPolicy::Marker, 0));
        seq.start();
        let (_, acts) = seq.push(tips(300, 300, 300));
        assert_eq!(polls(&acts), vec![PollRequest::Range { from: 100, to: 199 }]);
        let (_, acts) = seq.push(Input::Poll(Poll::RangeTooLarge { from: 100, to: 199 }));
        assert_eq!(polls(&acts), vec![PollRequest::Range { from: 100, to: 149 }]);
        assert_eq!(seq.cursors().poll_chunk, 50);
    }

    #[test]
    fn huge_poll_chunk_is_capped_by_the_tip() {
        let mut o = opts(CommitState::Finalized, SealPolicy::Marker, 0);
        o.poll_chunk = u64::MAX;
        let mut seq = Sequencer::new(100, o);
        seq.start();
        let (_, acts) = seq.push(tips(300, 300, 300));
        assert_eq!(polls(&acts), vec![PollRequest::Range { from: 100, to: 300 }]);
    }

    #[test]
    fn subscription_data_far_above_the_finalized_layer_is_not_buffered() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 1));
        seq.start();
        // Catching up from far behind: the live blocks are polled later.
        for h in 10_000..10_100 {
            feed(
                &mut seq,
                vec![
                    header(0, h, hash(h), hash(h - 1), CommitState::Proposed),
                    log(0, h, hash(h), CommitState::Proposed, 0),
                    marker(0, h, hash(h)),
                ],
            );
        }
        assert!(seq.candidates.is_empty(), "{} heights buffered", seq.candidates.len());
    }

    #[test]
    fn marker_seals_proposed_block() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 1));
        seq.start();
        seq.push(tips(99, 98, 97));
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
                log(0, 100, A, CommitState::Proposed, 1),
            ],
        );
        assert!(outs.is_empty(), "not sealed yet");
        let (outs, _) = seq.push(marker(0, 100, A));
        assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, false, 2)]);

        // Voted re-send: nothing new.
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Voted),
                log(0, 100, A, CommitState::Voted, 0),
                log(0, 100, A, CommitState::Voted, 1),
            ],
        );
        assert!(outs.is_empty());

        // Finalized re-send, sealed by the next header (R2).
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Finalized),
                log(0, 100, A, CommitState::Finalized, 0),
                log(0, 100, A, CommitState::Finalized, 1),
            ],
        );
        assert_eq!(emitted(&outs), vec![(100, CommitState::Finalized, false, 2)]);
        assert_eq!(seq.cursors().next_finalized, 101);
    }

    #[test]
    fn next_header_seals_without_marker() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::NextHeader, 1));
        seq.start();
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
                marker(0, 100, A), // ignored: no marker subscription semantics needed
            ],
        );
        // Marker still seals when present; test the header path instead.
        let _ = outs;
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::NextHeader, 1));
        seq.start();
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
            ],
        );
        assert!(outs.is_empty());
        let (outs, _) = seq.push(header(0, 101, B, A, CommitState::Proposed));
        assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, false, 1)]);
        // A repeated header of the current run does not seal it (R2).
        let (outs, _) = feed(
            &mut seq,
            vec![
                log(0, 101, B, CommitState::Proposed, 0),
                header(0, 101, B, A, CommitState::Proposed),
            ],
        );
        assert!(outs.is_empty());
        let (outs, _) = seq.push(log(0, 102, C, CommitState::Proposed, 0));
        assert_eq!(emitted(&outs), vec![(101, CommitState::Proposed, false, 1)]);
    }

    #[test]
    fn bloom_negative_header_needs_no_logs() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 1));
        seq.start();
        let (outs, _) = seq.push(Input::Ws(Ws::Header {
            src: 0,
            height: 100,
            hash: A,
            parent: hash(99),
            id: A,
            state: CommitState::Proposed,
            timestamp: 1,
            may_have_logs: false,
        }));
        assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, false, 0)]);
    }

    #[test]
    fn bloom_false_positive_is_sealed_by_the_next_header() {
        for seal in [SealPolicy::Marker, SealPolicy::NextHeader] {
            let mut seq = Sequencer::new(100, opts(CommitState::Proposed, seal, 1));
            seq.start();
            // 100's bloom matches the exchange, but no log follows its header.
            let (outs, _) = feed(
                &mut seq,
                vec![
                    header(0, 100, A, hash(99), CommitState::Proposed),
                    header(0, 101, B, A, CommitState::Proposed),
                ],
            );
            assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, false, 0)], "{seal:?}");
        }
    }

    #[test]
    fn competing_proposal_supersedes_and_parent_check_filters_descendants() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 1));
        seq.start();
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
                marker(0, 100, A),
                header(0, 101, B, A, CommitState::Proposed),
                log(0, 101, B, CommitState::Proposed, 0),
                marker(0, 101, B),
            ],
        );
        assert_eq!(
            emitted(&outs),
            vec![(100, CommitState::Proposed, false, 1), (101, CommitState::Proposed, false, 1)]
        );
        // A second proposal at 100.
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, C, hash(99), CommitState::Proposed),
                log(0, 100, C, CommitState::Proposed, 0),
                log(0, 100, C, CommitState::Proposed, 1),
                marker(0, 100, C),
            ],
        );
        assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, true, 2)]);
        assert_eq!(seq.cursors().next_speculative, 101);
        // The old 101 (child of A) is not re-emitted on top of C.
        let (outs, _) = seq.push(tips(101, 99, 98));
        assert!(outs.is_empty());
        // Finalized at 100 is C: no supersede flag; then 101 finalized as a
        // child of C.
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, C, hash(99), CommitState::Finalized),
                log(0, 100, C, CommitState::Finalized, 0),
                log(0, 100, C, CommitState::Finalized, 1),
                header(0, 101, hash(1011), C, CommitState::Finalized),
                log(0, 101, hash(1011), CommitState::Finalized, 0),
                header(0, 102, hash(1021), hash(1011), CommitState::Proposed),
            ],
        );
        assert_eq!(
            emitted(&outs),
            vec![(100, CommitState::Finalized, false, 2), (101, CommitState::Finalized, false, 1)]
        );
    }

    #[test]
    fn finalized_hash_differing_from_speculative_is_marked_superseding() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 1));
        seq.start();
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
                marker(0, 100, A),
            ],
        );
        assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, false, 1)]);
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, B, hash(99), CommitState::Finalized),
                log(0, 100, B, CommitState::Finalized, 0),
                log(0, 100, B, CommitState::Finalized, 1),
                header(0, 101, C, B, CommitState::Proposed),
            ],
        );
        assert_eq!(emitted(&outs), vec![(100, CommitState::Finalized, true, 2)]);
        assert_eq!(seq.cursors().next_speculative, 101);
    }

    #[test]
    fn lost_connection_leaves_block_incomplete_and_polling_completes_it() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 1));
        seq.start();
        seq.push(tips(99, 98, 97));
        let (outs, acts) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
                Input::Ws(Ws::Closed { src: 0 }),
            ],
        );
        assert!(outs.is_empty());
        assert!(tips_request(&acts).is_some(), "{acts:?}");
        // A header after reconnect must not seal the cut run.
        let (outs, _) = feed(
            &mut seq,
            vec![
                Input::Ws(Ws::Reconnected { src: 0 }),
                header(0, 101, B, A, CommitState::Proposed),
            ],
        );
        assert!(outs.is_empty());
        let (outs, acts) = seq.push(tips(101, 100, 100));
        assert!(outs.is_empty());
        assert_eq!(polls(&acts), vec![PollRequest::Block(100)]);
        let (outs, _) = seq.push(polled(100, A, 3));
        assert_eq!(emitted(&outs), vec![(100, CommitState::Finalized, false, 3)]);
    }

    #[test]
    fn quiescent_does_not_seal_a_run_cut_by_a_disconnect() {
        let quiet = Duration::from_millis(10);
        let mut seq =
            Sequencer::new(100, opts(CommitState::Voted, SealPolicy::Quiescent { quiet }, 1));
        seq.start();
        let (_, acts) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
                Input::Ws(Ws::Closed { src: 0 }),
            ],
        );
        let tick = acts
            .iter()
            .filter_map(|a| match a {
                Action::Timer(_, t @ Tick::Quiet { .. }) => Some(*t),
                _ => None,
            })
            .next_back()
            .unwrap();
        let (outs, _) = seq.push(Input::Tick(tick));
        assert!(outs.is_empty());
        // The tips name the block: it is fetched, not emitted half-complete.
        let named = Tips { latest: Tip::at(100), safe: Tip::at(100), finalized: tip(100) };
        let named = Tips { finalized: Tip { hash: Some(A), ..named.finalized }, ..named };
        let (outs, acts) = seq.push(Input::Poll(Poll::Tips(named)));
        assert!(outs.is_empty(), "{:?}", emitted(&outs));
        assert_eq!(polls(&acts), vec![PollRequest::Block(100)]);
    }

    #[test]
    fn finalized_header_off_the_finalized_chain_is_not_emitted() {
        let mut seq = Sequencer::new(100, opts(CommitState::Voted, SealPolicy::Marker, 1));
        seq.start();
        let (outs, _) = seq.push(polled(100, hash(100), 1));
        assert!(outs.is_empty());
        let (outs, _) = seq.push(named_tips(100, 100, 100));
        assert_eq!(emitted(&outs), vec![(100, CommitState::Finalized, false, 1)]);
        // A Finalized header at 101 that does not build on 100, with a bloom
        // that needs no logs: polled instead of emitted.
        let (outs, acts) = seq.push(Input::Ws(Ws::Header {
            src: 0,
            height: 101,
            hash: A,
            parent: B,
            id: A,
            state: CommitState::Finalized,
            timestamp: 1,
            may_have_logs: false,
        }));
        assert!(outs.is_empty(), "{:?}", emitted(&outs));
        assert!(polls(&acts).contains(&PollRequest::Block(101)), "{acts:?}");
        let (outs, _) = seq.push(polled(101, hash(101), 2));
        assert!(outs.is_empty(), "fetched before 101 was known final");
        let (outs, _) = seq.push(named_tips(101, 101, 101));
        assert_eq!(emitted(&outs), vec![(101, CommitState::Finalized, false, 2)]);

        // The polling provider itself serving a finalized block off the
        // emitted chain cannot be repaired: the stream ends.
        let (_, _) = seq.push(named_tips(102, 102, 102));
        let off_chain = Input::Poll(Poll::Block(PolledBlock {
            header: BlockHeader { number: 102, hash: hash(102), parent: C, timestamp: 1020 },
            events: vec![],
        }));
        let (outs, _) = seq.push(off_chain);
        assert!(
            matches!(outs.as_slice(), [Output::Error(DexError::BlockSuperseded(101))]),
            "{outs:?}"
        );
    }

    #[test]
    fn logs_of_a_run_joined_midway_never_seal_it() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 1));
        seq.start();
        // Subscribed in the middle of 100's run: its header and first log
        // went out before.
        let (outs, _) = feed(
            &mut seq,
            vec![
                log(0, 100, A, CommitState::Proposed, 1),
                marker(0, 100, A),
                header(0, 101, B, A, CommitState::Proposed),
            ],
        );
        assert!(outs.is_empty(), "{:?}", emitted(&outs));
        // 100's Voted header supplies the header the Proposed run lacked,
        // but not the log that went out before the subscription.
        let (outs, _) = seq.push(header(0, 100, A, hash(99), CommitState::Voted));
        assert!(outs.is_empty(), "{:?}", emitted(&outs));
        // The Voted run is whole and seals it.
        let (outs, _) = feed(
            &mut seq,
            vec![
                log(0, 100, A, CommitState::Voted, 0),
                log(0, 100, A, CommitState::Voted, 1),
                header(0, 101, B, A, CommitState::Voted),
            ],
        );
        // 101's header was followed by another header: it has no logs.
        assert_eq!(
            emitted(&outs),
            vec![(100, CommitState::Voted, false, 2), (101, CommitState::Voted, false, 0)]
        );
    }

    #[test]
    fn finalized_pick_prefers_the_complete_tip_named_candidate() {
        for _ in 0..16 {
            let mut seq = Sequencer::new(100, opts(CommitState::Voted, SealPolicy::Marker, 1));
            seq.start();
            // An incomplete subscribed Finalized candidate, and the polled
            // block the tips name.
            seq.push(header(0, 100, A, hash(99), CommitState::Finalized));
            seq.push(polled(100, hash(100), 2));
            let (outs, _) = seq.push(named_tips(100, 100, 100));
            assert_eq!(emitted(&outs), vec![(100, CommitState::Finalized, false, 2)]);
        }
    }

    #[test]
    fn voted_skipped_by_node_yields_finalized_only() {
        let mut seq = Sequencer::new(100, opts(CommitState::Voted, SealPolicy::Marker, 1));
        seq.start();
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
                marker(0, 100, A),
                header(0, 101, B, A, CommitState::Proposed),
            ],
        );
        assert!(outs.is_empty(), "Proposed is below min");
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Finalized),
                log(0, 100, A, CommitState::Finalized, 0),
                header(0, 102, C, B, CommitState::Proposed),
            ],
        );
        assert_eq!(emitted(&outs), vec![(100, CommitState::Finalized, false, 1)]);
    }

    #[test]
    fn same_block_from_two_sources_is_emitted_once() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 2));
        seq.start();
        let (outs, _) = feed(
            &mut seq,
            vec![
                header(1, 100, A, hash(99), CommitState::Proposed),
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(1, 100, A, CommitState::Proposed, 0),
                log(0, 100, A, CommitState::Proposed, 0),
                marker(1, 100, A),
                marker(0, 100, A),
            ],
        );
        assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, false, 1)]);
    }

    #[test]
    fn watchdog_rebuilds_a_silent_source() {
        let mut seq = Sequencer::new(100, opts(CommitState::Voted, SealPolicy::Marker, 1));
        seq.start();
        seq.push(Input::Ws(Ws::Reconnected { src: 0 }));
        let mut reconnects = 0;
        for _ in 0..2 {
            let (_, acts) = seq.push(Input::Tick(Tick::Watchdog));
            reconnects += acts
                .iter()
                .filter(|a| matches!(a, Action::Reconnect(0)))
                .count();
        }
        assert_eq!(reconnects, 1);
    }

    #[test]
    fn tips_accelerate_and_cover_the_speculative_tip_while_polling_drives() {
        let mut seq = Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Marker, 1));
        // Nothing connected yet: polling drives.
        let start = seq.start();
        assert_eq!(tips_request(&start), Some(Some(CommitState::Proposed)));
        assert_eq!(tips_timer(&start), Some(Duration::from_millis(100)));
        seq.push(tips(101, 100, 99));
        // A header marks the source alive: polling backs the subscription.
        seq.push(header(0, 100, A, hash(99), CommitState::Proposed));
        let (_, acts) = seq.push(Input::Tick(Tick::Tip));
        assert_eq!(tips_request(&acts), Some(None));
        assert_eq!(tips_timer(&acts), Some(Duration::from_millis(500)));
        seq.push(tips(101, 100, 99));
        // The only subscription dies: polling drives again, at once.
        let (_, acts) = seq.push(Input::Ws(Ws::Closed { src: 0 }));
        assert_eq!(tips_request(&acts), Some(Some(CommitState::Proposed)));
        seq.push(tips(101, 100, 99));
        let (_, acts) = seq.push(Input::Tick(Tick::Tip));
        assert_eq!(tips_timer(&acts), Some(Duration::from_millis(100)));
        // Back after a reconnect.
        seq.push(Input::Ws(Ws::Reconnected { src: 0 }));
        let (_, acts) = seq.push(Input::Tick(Tick::Tip));
        assert_eq!(tips_timer(&acts), Some(Duration::from_millis(500)));
    }

    #[test]
    fn quiescent_seals_after_quiet_period_and_late_log_re_emits() {
        let quiet = Duration::from_millis(10);
        let mut seq =
            Sequencer::new(100, opts(CommitState::Proposed, SealPolicy::Quiescent { quiet }, 1));
        seq.start();
        let (outs, acts) = feed(
            &mut seq,
            vec![
                header(0, 100, A, hash(99), CommitState::Proposed),
                log(0, 100, A, CommitState::Proposed, 0),
            ],
        );
        assert!(outs.is_empty());
        let ticks: Vec<Tick> = acts
            .iter()
            .filter_map(|a| match a {
                Action::Timer(_, t @ Tick::Quiet { .. }) => Some(*t),
                _ => None,
            })
            .collect();
        assert_eq!(ticks.len(), 2);
        // The first timer (token from the header) is stale, the second fires.
        let (outs, _) = seq.push(Input::Tick(ticks[0]));
        assert!(outs.is_empty());
        let (outs, _) = seq.push(Input::Tick(ticks[1]));
        assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, false, 1)]);
        // A late log re-emits the block as superseding once quiet again.
        let (_, acts) = seq.push(log(0, 100, A, CommitState::Proposed, 1));
        let tick = acts
            .iter()
            .find_map(|a| match a {
                Action::Timer(_, t @ Tick::Quiet { .. }) => Some(*t),
                _ => None,
            })
            .unwrap();
        let (outs, _) = seq.push(Input::Tick(tick));
        assert_eq!(emitted(&outs), vec![(100, CommitState::Proposed, true, 2)]);
    }
}
