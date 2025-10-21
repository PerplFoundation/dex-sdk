use fastnum::{D256, UD64, UD128};

use super::num;
use crate::{abi::dex::Exchange::PositionInfo, types};

#[derive(Clone, Copy, Debug)]
pub enum PositionType {
    Long = 0,
    Short = 1,
}

/// Open perpetual contract position.
#[derive(Clone, Debug)]
pub struct Position {
    instant: types::StateInstant,
    perpetual_id: types::PerpetualId,
    account_id: types::AccountId,
    r#type: PositionType,
    entry_price: UD64, // SC allocates 32 bits
    size: UD64,        // SC allocates 40 bits
    deposit: UD128,    // SC allocates 80 bits
    pnl: D256,
    delta_pnl: D256,
    premium_pnl: D256,
}

impl Position {
    pub(crate) fn new(
        instant: types::StateInstant,
        perpetual_id: types::PerpetualId,
        info: &PositionInfo,
        collateral_converter: num::Converter,
        price_converter: num::Converter,
        size_converter: num::Converter,
    ) -> Self {
        Self {
            instant,
            perpetual_id,
            account_id: info.accountId.to(),
            r#type: info.positionType.into(),
            entry_price: price_converter.from_unsigned(info.pricePNS),
            size: size_converter.from_unsigned(info.lotLNS),
            deposit: collateral_converter.from_unsigned(info.depositCNS),
            pnl: collateral_converter.from_signed(info.pnlCNS),
            delta_pnl: collateral_converter.from_signed(info.deltaPnlCNS),
            premium_pnl: collateral_converter.from_signed(info.premiumPnlCNS),
        }
    }

    /// Instant the position state is consistent with or was last updated at.
    pub fn instant(&self) -> types::StateInstant {
        self.instant
    }

    /// ID of the perpetual contract.
    pub fn perpetual_id(&self) -> types::PerpetualId {
        self.perpetual_id
    }

    /// ID of the account holding the position.
    pub fn account_id(&self) -> types::AccountId {
        self.account_id
    }

    /// Type of the position.
    pub fn r#type(&self) -> PositionType {
        self.r#type
    }

    /// Position entry price.
    pub fn entry_price(&self) -> UD64 {
        self.entry_price
    }

    /// Size of the position.
    pub fn size(&self) -> UD64 {
        self.size
    }

    /// Deposit (margin) locked in the position.
    pub fn deposit(&self) -> UD128 {
        self.deposit
    }

    /// Unrealized PnL (Delta PnL + Premium PnL) of the position.
    pub fn pnl(&self) -> D256 {
        self.pnl
    }

    /// Unrealized Delta PnL of the position.
    pub fn delta_pnl(&self) -> D256 {
        self.delta_pnl
    }

    /// Unrealized Premium PnL of the position.
    pub fn premium_pnl(&self) -> D256 {
        self.premium_pnl
    }
}

impl From<u8> for PositionType {
    fn from(value: u8) -> Self {
        match value {
            0 => PositionType::Long,
            1 => PositionType::Short,
            _ => unreachable!(),
        }
    }
}
