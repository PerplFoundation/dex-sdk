use fastnum::{D256, UD64, UD128};

use super::num;
use crate::{abi::dex::Exchange::PositionInfo, types};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionType {
    Long = 0,
    Short = 1,
}

/// Open perpetual contract position.
#[derive(Clone, Debug)]
pub struct Position {
    instant: types::StateInstant,
    funding_instant: types::StateInstant,
    perpetual_id: types::PerpetualId,
    account_id: types::AccountId,
    r#type: PositionType,
    entry_price: UD64, // SC allocates 32 bits
    size: UD64,        // SC allocates 40 bits
    deposit: UD128,    // SC allocates 80 bits
    delta_pnl: D256,   // SC calculations and ABI use 256 bits
    premium_pnl: D256, // SC calculations and ABI use 256 bits
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
            funding_instant: instant,
            perpetual_id,
            account_id: info.accountId.to(),
            r#type: info.positionType.into(),
            entry_price: price_converter.from_unsigned(info.pricePNS),
            size: size_converter.from_unsigned(info.lotLNS),
            deposit: collateral_converter.from_unsigned(info.depositCNS),
            delta_pnl: collateral_converter.from_signed(info.deltaPnlCNS),
            premium_pnl: collateral_converter.from_signed(info.premiumPnlCNS),
        }
    }

    pub(crate) fn opened(
        instant: types::StateInstant,
        perpetual_id: types::PerpetualId,
        account_id: types::AccountId,
        r#type: PositionType,
        entry_price: UD64,
        size: UD64,
        deposit: UD128,
    ) -> Self {
        Self {
            instant,
            funding_instant: instant,
            perpetual_id,
            account_id,
            r#type,
            entry_price,
            size,
            deposit,
            delta_pnl: D256::ZERO,
            premium_pnl: D256::ZERO,
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

    /// Collateral deposit / margin locked in the position.
    pub fn deposit(&self) -> UD128 {
        self.deposit
    }

    /// Unrealized Delta PnL of the position.
    pub fn delta_pnl(&self) -> D256 {
        self.delta_pnl
    }

    /// Unrealized Premium PnL of the position.
    pub fn premium_pnl(&self) -> D256 {
        self.premium_pnl
    }

    /// Unrealized PnL of the position.
    pub fn pnl(&self) -> D256 {
        self.delta_pnl + self.premium_pnl
    }

    pub(crate) fn update_type(&mut self, instant: types::StateInstant, r#type: PositionType) {
        self.r#type = r#type;
        self.instant = instant;
    }

    pub(crate) fn update_entry_price(&mut self, instant: types::StateInstant, entry_price: UD64) {
        self.entry_price = entry_price;
        self.instant = instant;
    }

    pub(crate) fn update_size(&mut self, instant: types::StateInstant, size: UD64) {
        self.size = size;
        self.instant = instant;
    }

    pub(crate) fn update_deposit(&mut self, instant: types::StateInstant, deposit: UD128) {
        self.deposit = deposit;
        self.instant = instant;
    }

    pub(crate) fn update_delta_pnl(&mut self, instant: types::StateInstant, delta_pnl: D256) {
        self.delta_pnl = delta_pnl;
        self.instant = instant;
    }

    pub(crate) fn update_premium_pnl(&mut self, instant: types::StateInstant, premium_pnl: D256) {
        self.premium_pnl = premium_pnl;
        self.instant = instant;
        self.funding_instant = instant;
    }

    pub(crate) fn apply_mark_price(&mut self, instant: types::StateInstant, mark_price: UD64) {
        let sign = if self.r#type.is_long() {
            D256::ONE
        } else {
            D256::ONE.neg()
        };
        self.delta_pnl = sign
            * (mark_price.resize().to_signed() - self.entry_price.resize().to_signed())
            * self.size.resize().to_signed();
        self.instant = instant;
    }

    pub(crate) fn apply_funding_payment(
        &mut self,
        instant: types::StateInstant,
        payment_per_unit: D256,
    ) -> bool {
        // Updating premium PnL only if it wasn't updated at the same instant
        if self.funding_instant >= instant {
            return false;
        }

        // Positive funding payment means longs pay shorts
        let sign = if self.r#type.is_long() {
            D256::ONE.neg()
        } else {
            D256::ONE
        };
        self.premium_pnl += sign * payment_per_unit * self.size.resize().to_signed();
        self.instant = instant;
        self.funding_instant = instant;
        true
    }
}

impl PositionType {
    pub fn is_long(&self) -> bool {
        matches!(self, PositionType::Long)
    }

    pub fn is_short(&self) -> bool {
        matches!(self, PositionType::Short)
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

#[cfg(test)]
mod tests {
    use fastnum::{dec256, udec64};

    use crate::types::StateInstant;

    use super::*;

    #[test]
    fn test_apply_mark_price() {
        let mut pos = Position::opened(
            StateInstant::default(),
            1,
            1,
            PositionType::Long,
            udec64!(100),
            udec64!(10),
            UD128::ZERO,
        );

        pos.apply_mark_price(StateInstant::default(), udec64!(150));
        assert_eq!(pos.delta_pnl(), dec256!(500));

        pos.apply_mark_price(StateInstant::default(), udec64!(50));
        assert_eq!(pos.delta_pnl(), dec256!(-500));

        let mut pos = Position::opened(
            StateInstant::default(),
            1,
            1,
            PositionType::Short,
            udec64!(100),
            udec64!(10),
            UD128::ZERO,
        );
        pos.apply_mark_price(StateInstant::default(), udec64!(150));
        assert_eq!(pos.delta_pnl(), dec256!(-500));

        pos.apply_mark_price(StateInstant::default(), udec64!(50));
        assert_eq!(pos.delta_pnl(), dec256!(500));
    }

    #[test]
    fn test_apply_funding_payment() {
        let (i0, i1, i2) = (
            StateInstant::default(),
            StateInstant::new(1, 1),
            StateInstant::new(2, 2),
        );
        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Long,
            udec64!(100),
            udec64!(10),
            UD128::ZERO,
        );

        assert!(pos.apply_funding_payment(i1, dec256!(5)));
        assert_eq!(pos.premium_pnl(), dec256!(-50));

        assert!(pos.apply_funding_payment(i2, dec256!(-10)));
        assert_eq!(pos.premium_pnl(), dec256!(50));

        assert!(!pos.apply_funding_payment(i2, dec256!(-10)));

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Short,
            udec64!(100),
            udec64!(10),
            UD128::ZERO,
        );

        pos.apply_funding_payment(i1, dec256!(5));
        assert_eq!(pos.premium_pnl(), dec256!(50));

        pos.apply_funding_payment(i2, dec256!(-10));
        assert_eq!(pos.premium_pnl(), dec256!(-50));
    }
}
