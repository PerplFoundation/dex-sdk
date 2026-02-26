use alloy::primitives::{Address, U256};
use fastnum::{D256, UD128};

use super::*;
use crate::{
    abi::dex::Exchange::{AccountInfo, PositionBitMap},
    types,
};

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
    pub fn instant(&self) -> types::StateInstant { self.instant }

    /// ID of the account.
    pub fn id(&self) -> types::AccountId { self.id }

    /// Account address.
    pub fn address(&self) -> Address { self.address }

    /// The current balance of collateral tokens in this account,
    /// not including any open positions.
    pub fn balance(&self) -> UD128 { self.balance }

    /// The balance of collateral tokens locked by existing orders for this
    /// account.
    /// If this value exceeds [`Self::balance`], new Open* orders cannot be
    /// placed.
    pub fn locked_balance(&self) -> UD128 { self.locked_balance }

    /// The balance of collateral tokens available for trading.
    pub fn available_balance(&self) -> UD128 {
        if self.locked_balance > self.balance {
            // Valid scenario from the smart contract perspective
            return UD128::ZERO;
        }
        self.balance - self.locked_balance
    }

    /// Total unrealized PnL of all positions of the account.
    pub fn unrealized_pnl(&self) -> D256 { self.positions.values().map(|p| p.pnl()).sum() }

    /// Indicator of the account being frozen.
    pub fn frozen(&self) -> bool { self.frozen }

    /// Positions the account has, up to one per each perpetual contract.
    pub fn positions(&self) -> &HashMap<types::PerpetualId, position::Position> { &self.positions }

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

#[cfg(feature = "display")]
impl std::fmt::Display for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use colored::Colorize;
        use tabled::{Table, settings::Style};

        if !self.address.is_zero() {
            // Full account state is known
            let pnl = self.unrealized_pnl();
            writeln!(
                f,
                "{} ({}) {}\n    Balance: {} | Available: {} | Locked: {} | Unrealized PnL: {}",
                format!("Account #{}", self.id).blue(),
                self.address,
                if self.frozen { "FROZEN ".bright_red() } else { Default::default() },
                self.balance,
                self.available_balance().to_string().green(),
                self.locked_balance,
                if pnl.is_negative() { pnl.to_string().red() } else { pnl.to_string().green() },
            )?;
        } else {
            // Only ID is known
            writeln!(f, "{}", format!("Account #{}", self.id).blue())?;
        }

        // Render positions in alternate mode
        if f.alternate() {
            let mut positions: Vec<_> = self.positions().values().collect();
            positions.sort_by_key(|p| p.perpetual_id());
            let mut positions_table = Table::new(positions);
            positions_table.with(Style::sharp());
            positions_table.fmt(f)
        } else {
            Ok(())
        }
    }
}

/// Returns IDs of perpetuals with positions according to [`PositionBitMap`].
pub(crate) fn perpetuals_with_position(bitmap: &PositionBitMap) -> Vec<types::PerpetualId> {
    let banks = vec![
        (0, (0..U256::BITS - 3), bitmap.bank1, bitmap.bank1.count_ones()),
        (253, (0..U256::BITS), bitmap.bank2, bitmap.bank2.count_ones()),
        (509, (0..U256::BITS), bitmap.bank3, bitmap.bank3.count_ones()),
        (765, (0..U256::BITS), bitmap.bank4, bitmap.bank4.count_ones()),
    ];
    banks
        .into_iter()
        .filter(|(_, _, _, count)| *count > 0)
        .flat_map(|(offs, range, bank, _)| {
            range.filter_map(move |i| bank.bit(i).then_some((offs + i) as types::PerpetualId))
        })
        .collect()
}
