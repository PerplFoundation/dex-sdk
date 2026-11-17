use alloy::primitives::{B256, TxHash};

/// Consensus commit state of a Monad block.
///
/// Monad executes a block as soon as it is proposed and announces the same
/// block again at every later stage of consensus. The order of the variants is
/// the order of consensus progress, so `Voted > Proposed` and a block tagged
/// `Finalized` satisfies any `min_commit_state` the consumer asked for.
///
/// * `Proposed`: executed by the node, not yet voted on. Can be abandoned in
///   favour of another proposal at the same height, so anything derived from it
///   is speculative.
/// * `Voted`: carries a quorum certificate (`safe` block tag). The node may
///   skip announcing this state when it lags behind consensus, so a consumer
///   that asks for `Voted` has to accept a `Finalized` update as its first
///   sight of a block.
/// * `Finalized`: irreversible (`finalized` block tag).
/// * `Verified`: finalized and its state root attested by the delayed execution
///   results. Never produced by the SDK streams, which normalise it to
///   `Finalized`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "ws", derive(serde::Deserialize))]
pub enum CommitState {
    Proposed,
    Voted,
    Finalized,
    Verified,
}

impl CommitState {
    /// Block tag the state corresponds to for polling (`latest`, `safe`,
    /// `finalized`).
    pub fn block_tag(self) -> alloy::eips::BlockNumberOrTag {
        use alloy::eips::BlockNumberOrTag;
        match self {
            Self::Proposed => BlockNumberOrTag::Latest,
            Self::Voted => BlockNumberOrTag::Safe,
            Self::Finalized | Self::Verified => BlockNumberOrTag::Finalized,
        }
    }

    /// `Verified` folded into `Finalized`, the highest state the streams emit.
    pub(crate) fn normalized(self) -> Self {
        match self {
            Self::Verified => Self::Finalized,
            other => other,
        }
    }
}

impl std::fmt::Display for CommitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Proposed => "proposed",
            Self::Voted => "voted",
            Self::Finalized => "finalized",
            Self::Verified => "verified",
        })
    }
}

impl std::str::FromStr for CommitState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "proposed" | "latest" => Ok(Self::Proposed),
            "voted" | "safe" => Ok(Self::Voted),
            "finalized" => Ok(Self::Finalized),
            "verified" => Ok(Self::Verified),
            other => Err(format!("unknown commit state: {other}")),
        }
    }
}

/// Events from a specific block.
///
/// Besides the block instant and the events themselves, a batch carries the
/// block's identity and consensus progress, see [`CommitState`]. Batches built
/// by hand through [`BlockEvents::new`] default to `Finalized` with no hashes,
/// which is what every pre-existing consumer assumed, so nothing changes for
/// code that does not look at the tags.
#[derive(Clone, Debug)]
pub struct BlockEvents<T> {
    instant: super::StateInstant,
    events: Vec<T>,
    commit_state: CommitState,
    block_hash: Option<B256>,
    block_id: Option<B256>,
    supersedes: bool,
}

/// Event along with transaction context.
#[derive(Clone, Debug)]
pub struct EventContext<T> {
    pub(crate) tx_hash: TxHash,
    pub(crate) tx_index: u64,
    pub(crate) log_index: u64,
    pub(crate) event: T,
}

impl<T> BlockEvents<T> {
    pub fn new(instant: super::StateInstant, events: Vec<T>) -> Self {
        Self {
            instant,
            events,
            commit_state: CommitState::Finalized,
            block_hash: None,
            block_id: None,
            supersedes: false,
        }
    }

    /// Instant the events produced at.
    pub fn instant(&self) -> super::StateInstant { self.instant }

    /// Raw exchange events
    pub fn events(&self) -> &[T] { &self.events }

    /// Consensus progress of the block the events came from.
    pub fn commit_state(&self) -> CommitState { self.commit_state }

    /// Execution hash of the block, `None` for hand-built batches.
    pub fn block_hash(&self) -> Option<B256> { self.block_hash }

    /// Monad consensus block id, only known for blocks that came in through a
    /// subscription. Two proposals at the same height always differ in id.
    pub fn block_id(&self) -> Option<B256> { self.block_id }

    /// `true` when this batch replaces an earlier, speculative batch at the
    /// same height (and implicitly every speculative batch above it): the
    /// earlier proposal was abandoned by consensus, or the earlier batch was
    /// incomplete.
    pub fn supersedes(&self) -> bool { self.supersedes }

    /// Same batch, tagged with the given commit state.
    pub fn with_commit_state(mut self, state: CommitState) -> Self {
        self.commit_state = state;
        self
    }

    /// Same batch, tagged with the execution block hash.
    pub fn with_block_hash(mut self, hash: B256) -> Self {
        self.block_hash = Some(hash);
        self
    }

    /// Same batch, tagged with the consensus block id.
    pub fn with_block_id(mut self, id: B256) -> Self {
        self.block_id = Some(id);
        self
    }

    /// Same batch, marked as superseding (or not) earlier speculative batches.
    pub fn superseding(mut self, supersedes: bool) -> Self {
        self.supersedes = supersedes;
        self
    }

    /// Same events, carrying the instant and every tag of `other`.
    pub fn with_tags_of<U>(mut self, other: &BlockEvents<U>) -> Self {
        self.commit_state = other.commit_state;
        self.block_hash = other.block_hash;
        self.block_id = other.block_id;
        self.supersedes = other.supersedes;
        self
    }

    /// Maps the events into another type, keeping the instant and every tag.
    pub fn map_events<O>(&self, f: impl FnOnce(&[T]) -> Vec<O>) -> BlockEvents<O> {
        BlockEvents {
            instant: self.instant,
            events: f(&self.events),
            commit_state: self.commit_state,
            block_hash: self.block_hash,
            block_id: self.block_id,
            supersedes: self.supersedes,
        }
    }

    /// Block number of the batch.
    pub fn block_number(&self) -> u64 { self.instant.block_number() }
}

impl<T> EventContext<T> {
    pub fn new(tx_hash: TxHash, tx_index: u64, log_index: u64, event: T) -> Self {
        Self { tx_hash, tx_index, log_index, event }
    }

    pub(crate) fn empty(event: T) -> Self {
        Self { tx_hash: TxHash::ZERO, tx_index: 0, log_index: 0, event }
    }

    pub fn tx_hash(&self) -> TxHash { self.tx_hash }

    pub fn tx_index(&self) -> u64 { self.tx_index }

    pub fn log_index(&self) -> u64 { self.log_index }

    pub fn event(&self) -> &T { &self.event }

    pub(crate) fn pass<O>(&self, other: O) -> EventContext<O> {
        EventContext {
            tx_hash: self.tx_hash,
            tx_index: self.tx_index,
            log_index: self.log_index,
            event: other,
        }
    }
}
