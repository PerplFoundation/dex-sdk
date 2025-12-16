mod event;
mod order;
mod request;

pub use event::*;
pub use order::{OrderSide, OrderType};
pub use request::{OrderRequest, RequestType};

/// ID of perpetual contract.
pub type PerpetualId = u32;

/// ID of exchange account.
pub type AccountId = u32;

/// Exchange internal ID of the order.
/// Unique only within particular perpetual contract at the
/// exact point in time.
pub type OrderId = u16;

/// Order request ID.
pub type RequestId = u64;

/// Instant in chain history the state/event is up to date with.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, Default)]
pub struct StateInstant {
    block_number: u64,
    block_timestamp: u64,
}

impl StateInstant {
    pub fn new(block_number: u64, block_timestamp: u64) -> Self {
        Self {
            block_number,
            block_timestamp,
        }
    }

    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    pub fn block_timestamp(&self) -> u64 {
        self.block_timestamp
    }
}
