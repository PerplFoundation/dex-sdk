mod event;
mod extension;
mod order;
mod request;
mod trade;

use std::{fmt::Display, str::FromStr};

use alloy::primitives::{Address, U256};
use chrono::{DateTime, Utc};
pub use event::*;
pub use extension::*;
pub use order::{OrderSide, OrderType};
pub use request::{
    DEFAULT_MAX_NEG_PNL_COLLAT_BPS, MAX_MATCHES, OrderField, OrderRequest, OrderRequestBuilder,
    OrderRequestBuilderError, RequestType,
};
pub use trade::*;

/// ID of perpetual contract.
pub type PerpetualId = u32;

/// Highest perpetual contract ID the exchange supports
/// (`C._MAX_CONTRACT_ID`), so the ID space is `0..=MAX_PERPETUAL_ID`.
pub const MAX_PERPETUAL_ID: PerpetualId = 1020;

/// ID of exchange account.
pub type AccountId = u32;

/// Fee tier of an exchange account, indexing a [`crate::state::FeeSchedule`].
/// Tier 0 is the base rate.
pub type FeeTier = u8;

/// Builder code attributed to an order. Zero means no builder.
pub type BuilderId = u8;

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

/// Order ID of a request, `None` for trigger order requests - their order IDs
/// might exceed `u16::MAX` and they are not supported by the SDK yet.
pub(crate) fn try_order_id(order_id: U256) -> Option<OrderId> {
    if order_id <= U256::from(u16::MAX) {
        std::num::NonZeroU16::new(order_id.to::<u16>())
    } else {
        None
    }
}

/// Narrows a request field the CALLER chose, not the contract.
///
/// `Uint::to()` panics on overflow, and every field of `OrderRequest`/`V2` is
/// the caller's `OrderDesc` echoed verbatim: `_emitOrderStartDelim` runs ahead
/// of `_validOrder` so the request is logged "regardless of failure mode", and
/// `execOrders(descs, revertOnFail)` is permissionless with the flag chosen by
/// the caller - so an order the contract rejected is skipped, not reverted, and
/// its log is mined. `lastExecutionBlock`, `maxMatches` and `leverageHdths` are
/// not bounded above at all, so even a valid order can carry a full-width one.
///
/// A panic here would take down the consuming application on a log that any
/// account can put on chain for the price of one transaction, so this saturates
/// instead - the same fail-soft choice [`try_order_id`] already makes.
pub(crate) fn narrow<T>(v: U256) -> T
where
    U256: alloy::primitives::ruint::UintTryTo<T>,
{
    v.saturating_to()
}

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
