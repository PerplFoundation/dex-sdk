mod event;
mod order;
mod request;
mod trade;

use std::{fmt::Display, str::FromStr};

use alloy::primitives::Address;
use chrono::{DateTime, Utc};
pub use event::*;
pub use order::{OrderSide, OrderType};
pub use request::{OrderRequest, RequestType};
pub use trade::*;

/// ID of perpetual contract.
pub type PerpetualId = u32;

/// ID of exchange account.
pub type AccountId = u32;

/// Account address or ID.
#[derive(Clone, Copy, Debug)]
pub enum AccountAddressOrID {
    Address(Address),
    ID(AccountId),
}

/// Exchange internal ID of the order.
/// Unique only within particular perpetual contract at the
/// exact point in time.
/// Note: The exchange uses 0 as NULL_ORDER_ID sentinel, so valid order IDs are
/// always non-zero.
pub type OrderId = std::num::NonZeroU16;

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
        Self { block_number, block_timestamp }
    }

    pub fn block_number(&self) -> u64 { self.block_number }

    pub fn block_timestamp(&self) -> u64 { self.block_timestamp }

    pub fn next(&self) -> Self {
        Self { block_number: self.block_number + 1, block_timestamp: self.block_timestamp }
    }
}

impl Display for StateInstant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ts = DateTime::<Utc>::from_timestamp(self.block_timestamp as i64, 0)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S");
        if self.block_number > 0 {
            write!(f, "#{} @ {}", self.block_number, ts)
        } else {
            write!(f, "{}", ts)
        }
    }
}

impl FromStr for AccountAddressOrID {
    type Err = crate::error::DexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(address) = Address::from_str(s) {
            return Ok(AccountAddressOrID::Address(address));
        }
        if let Ok(id) = AccountId::from_str(s) {
            return Ok(AccountAddressOrID::ID(id));
        }
        Err(crate::error::DexError::InvalidArgument(format!(
            "invalid account address or ID: {}",
            s
        )))
    }
}

impl TryFrom<String> for AccountAddressOrID {
    type Error = crate::error::DexError;

    fn try_from(value: String) -> Result<Self, Self::Error> { AccountAddressOrID::from_str(&value) }
}
