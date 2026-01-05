use super::*;
use crate::{
    abi::dex::Exchange::{AccountInfo, PositionBitMap},
    types,
};
use alloy::primitives::{Address, U256};
use fastnum::UD128;

/// Exchange account.
#[derive(Clone, derive_more::Debug)]
pub struct Account {
    instant: types::StateInstant,
    id: types::AccountId,
    address: Address,
    #[debug("{balance}")]
    balance: UD128, // SC allocates 80 bits
    #[debug("{locked_balance}")]
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

    pub(crate) fn from_event(
        instant: types::StateInstant,
        id: types::AccountId,
        address: Address,
    ) -> Self {
        Self {
            instant,
            id,
            address,
            balance: UD128::ZERO,
            locked_balance: UD128::ZERO,
            frozen: false,
            positions: HashMap::new(),
        }
    }

    pub(crate) fn from_position(instant: types::StateInstant, position: Position) -> Self {
        let account_id = position.account_id();
        let mut positions = HashMap::new();
        positions.insert(position.perpetual_id(), position);
        Self {
            instant,
            id: account_id,
            address: Address::ZERO,
            balance: UD128::ZERO,
            locked_balance: UD128::ZERO,
            frozen: false,
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

    pub(crate) fn update_frozen(&mut self, instant: types::StateInstant, frozen: bool) {
        self.frozen = frozen;
        self.instant = instant;
    }

    pub(crate) fn update_balance(&mut self, instant: types::StateInstant, balance: UD128) {
        self.balance = balance;
        self.instant = instant;
    }

    pub(crate) fn update_locked_balance(
        &mut self,
        instant: types::StateInstant,
        locked_balance: UD128,
    ) {
        self.locked_balance = locked_balance;
        self.instant = instant;
    }

    pub(crate) fn positions_mut(&mut self) -> &mut HashMap<types::PerpetualId, position::Position> {
        &mut self.positions
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
        .flat_map(|(offs, range, bank, _)| {
            range.filter_map(move |i| bank.bit(i).then_some((offs + i) as types::PerpetualId))
        })
        .collect()
}
