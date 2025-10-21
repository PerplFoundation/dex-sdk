use alloy::primitives::{Address, U256};
use fastnum::UD128;
use hashbrown::HashMap;

use super::*;
use crate::{
    abi::dex::Exchange::{AccountInfo, PositionBitMap},
    types,
};

/// Exchange account.
#[derive(Clone, Debug)]
pub struct Account {
    instant: types::StateInstant,
    id: types::AccountId,
    address: Address,
    balance: UD128,        // SC allocates 80 bits
    locked_balance: UD128, // SC allocates 80 bits
    frozen: bool,
    positions: HashMap<types::PerpetualId, Position>,
}

impl Account {
    pub(crate) fn new(
        instant: types::StateInstant,
        id: types::AccountId,
        info: &AccountInfo,
        positions: HashMap<types::PerpetualId, Position>,
        collateral_converter: num::Converter,
    ) -> Self {
        Self {
            instant,
            id,
            address: info.accountAddr,
            balance: collateral_converter.from_unsigned(info.balanceCNS),
            locked_balance: collateral_converter.from_unsigned(info.lockedBalanceCNS),
            frozen: info.frozen != 0,
            positions,
        }
    }

    /// Instant the account state is consistent with or was last updated at.
    pub fn instant(&self) -> types::StateInstant {
        self.instant
    }

    /// ID of the account.
    pub fn id(&self) -> types::AccountId {
        self.id
    }

    /// Account address.
    pub fn address(&self) -> Address {
        self.address
    }

    /// The current balance of collateral tokens in this account,
    /// not including any open positions.
    pub fn balance(&self) -> UD128 {
        self.balance
    }

    /// The balance of collateral tokens locked by existing orders for this
    /// account.
    /// If this value exceeds [`Self::balance`], new Open* orders cannot be
    /// placed.
    pub fn locked_balance(&self) -> UD128 {
        self.locked_balance
    }

    /// Indicator of the account being frozen.
    pub fn frozen(&self) -> bool {
        self.frozen
    }

    /// Positions the account has, up to one per each perpetual contract.
    pub fn positions(&self) -> &HashMap<types::PerpetualId, position::Position> {
        &self.positions
    }
}

/// Returns IDs of perpetuals with positions according to [`PositionBitMap`].
pub(crate) fn perpetuals_with_position(bitmap: &PositionBitMap) -> Vec<types::PerpetualId> {
    let banks = vec![
        (
            0,
            (0..U256::BITS - 3),
            bitmap.bank1,
            bitmap.bank1.count_ones(),
        ),
        (
            253,
            (0..U256::BITS),
            bitmap.bank2,
            bitmap.bank2.count_ones(),
        ),
        (
            509,
            (0..U256::BITS),
            bitmap.bank3,
            bitmap.bank3.count_ones(),
        ),
        (
            765,
            (0..U256::BITS),
            bitmap.bank4,
            bitmap.bank4.count_ones(),
        ),
    ];
    banks
        .into_iter()
        .filter(|(_, _, _, count)| *count > 0)
        .map(|(offs, range, bank, _)| {
            range.filter_map(move |i| bank.bit(i).then_some((offs + i) as types::PerpetualId))
        })
        .flatten()
        .collect()
}
