use alloy::primitives::TxHash;

/// Events from a specific block.
#[derive(Debug)]
pub struct BlockEvents<T> {
    instant: super::StateInstant,
    events: Vec<T>,
}

/// Event along with transaction context.
#[derive(Debug)]
pub struct EventContext<T> {
    pub(crate) tx_hash: TxHash,
    pub(crate) tx_index: u64,
    pub(crate) log_index: u64,
    pub(crate) event: T,
}

impl<T> BlockEvents<T> {
    pub(crate) fn new(instant: super::StateInstant, events: Vec<T>) -> Self {
        Self { instant, events }
    }

    /// Instant the events produced at.
    pub fn instant(&self) -> super::StateInstant {
        self.instant
    }

    /// Raw exchange events
    pub fn events(&self) -> &[T] {
        &self.events
    }
}

impl<T> EventContext<T> {
    pub(crate) fn new(tx_hash: TxHash, tx_index: u64, log_index: u64, event: T) -> Self {
        Self {
            tx_hash,
            tx_index,
            log_index,
            event,
        }
    }

    pub(crate) fn empty(event: T) -> Self {
        Self {
            tx_hash: TxHash::ZERO,
            tx_index: 0,
            log_index: 0,
            event,
        }
    }

    pub fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    pub fn tx_index(&self) -> u64 {
        self.tx_index
    }

    pub fn log_index(&self) -> u64 {
        self.log_index
    }

    pub fn event(&self) -> &T {
        &self.event
    }

    pub(crate) fn pass<O>(&self, other: O) -> EventContext<O> {
        EventContext {
            tx_hash: self.tx_hash,
            tx_index: self.tx_index,
            log_index: self.log_index,
            event: other,
        }
    }
}
