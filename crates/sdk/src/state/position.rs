use alloy::primitives::U256;
use fastnum::{D64, D256, UD64, UD128, udec64};

use super::num;
use crate::{abi::dex::Exchange::PositionInfoV2, types};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionType {
    Long = 0,
    Short = 1,
}

/// Open perpetual contract position.
#[derive(Clone, derive_more::Debug)]
pub struct Position {
    instant: types::StateInstant,
    funding_instant: types::StateInstant,
    perpetual_id: types::PerpetualId,
    account_id: types::AccountId,
    r#type: PositionType,
    #[debug("{entry_price}")]
    entry_price: UD64, // SC allocates 32 bits + 16 bits residue
    #[debug("{size}")]
    size: UD64, // SC allocates 40 bits
    #[debug("{deposit}")]
    deposit: UD128, // SC allocates 80 bits
    #[debug("{delta_pnl}")]
    delta_pnl: D256, // SC calculations and ABI use 256 bits
    #[debug("{premium_pnl}")]
    premium_pnl: D256, // SC calculations and ABI use 256 bits
    #[debug("{maintenance_margin_requirement}")]
    maintenance_margin_requirement: UD128,
}

impl Position {
    pub(crate) fn new(
        instant: types::StateInstant,
        perpetual_id: types::PerpetualId,
        info: &PositionInfoV2,
        collateral_converter: num::Converter,
        price_converter: num::Converter,
        size_converter: num::Converter,
        maintenance_margin: UD64,
    ) -> Self {
        let r#type = info.positionType.into();
        let entry_price = Self::effective_entry_price(
            r#type,
            info.pricePNS,
            info.priceResiduePNSQ16.to(),
            price_converter,
        );
        let size = size_converter.from_unsigned(info.lotLNS);
        Self {
            instant,
            funding_instant: instant,
            perpetual_id,
            account_id: info.accountId.to(),
            r#type,
            entry_price,
            size,
            deposit: collateral_converter.from_unsigned(info.depositCNS),
            delta_pnl: collateral_converter.from_signed(info.deltaPnlCNS),
            premium_pnl: collateral_converter.from_signed(info.premiumPnlCNS),
            maintenance_margin_requirement: entry_price.resize() * size.resize()
                / maintenance_margin.resize(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn opened(
        instant: types::StateInstant,
        perpetual_id: types::PerpetualId,
        account_id: types::AccountId,
        r#type: PositionType,
        price_pns: U256,
        price_residue_pnsq16: u32,
        price_converter: num::Converter,
        size: UD64,
        deposit: UD128,
        maintenance_margin: UD64,
    ) -> Self {
        let entry_price =
            Self::effective_entry_price(r#type, price_pns, price_residue_pnsq16, price_converter);
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
            maintenance_margin_requirement: entry_price.resize() * size.resize()
                / maintenance_margin.resize(),
        }
    }

    /// Instant the position state is consistent with or was last updated at.
    pub fn instant(&self) -> types::StateInstant { self.instant }

    /// ID of the perpetual contract.
    pub fn perpetual_id(&self) -> types::PerpetualId { self.perpetual_id }

    /// ID of the account holding the position.
    pub fn account_id(&self) -> types::AccountId { self.account_id }

    /// Type of the position.
    pub fn r#type(&self) -> PositionType { self.r#type }

    /// Position entry price, full precision - including 16 bit rounding residue
    pub fn entry_price(&self) -> UD64 { self.entry_price }

    /// Size of the position.
    pub fn size(&self) -> UD64 { self.size }

    /// Collateral deposit / margin locked in the position.
    pub fn deposit(&self) -> UD128 { self.deposit }

    /// Unrealized Delta PnL of the position.
    pub fn delta_pnl(&self) -> D256 { self.delta_pnl }

    /// Unrealized Premium PnL of the position.
    pub fn premium_pnl(&self) -> D256 { self.premium_pnl }

    /// Unrealized PnL of the position.
    pub fn pnl(&self) -> D256 { self.delta_pnl + self.premium_pnl }

    /// Maintenance margin requirement of the position.
    pub fn maintenance_margin_requirement(&self) -> UD128 { self.maintenance_margin_requirement }

    /// Liquidation price of the position.
    pub fn liquidation_price(&self) -> UD64 {
        let side = if self.r#type.is_long() { D256::ONE } else { D256::ONE.neg() };
        let liquidation_price = self.entry_price.to_signed()
            + (side
                * (self.maintenance_margin_requirement.to_signed().resize()
                    - self.deposit.to_signed().resize()
                    - self.premium_pnl)
                / self.size.to_signed().resize())
            .resize();
        liquidation_price.max(D64::ZERO).unsigned_abs()
    }

    /// Bankruptcy price of the position.
    pub fn bankruptcy_price(&self) -> UD64 {
        let side = if self.r#type.is_long() { D256::ONE } else { D256::ONE.neg() };
        let bankruptcy_price = self.entry_price.to_signed()
            - (side * (self.deposit.to_signed().resize() + self.premium_pnl)
                / self.size.to_signed().resize())
            .resize();
        bankruptcy_price.max(D64::ZERO).unsigned_abs()
    }

    pub(crate) fn update_type(&mut self, instant: types::StateInstant, r#type: PositionType) {
        self.r#type = r#type;
        self.instant = instant;
    }

    pub(crate) fn update_entry_price(
        &mut self,
        instant: types::StateInstant,
        price_pns: U256,
        price_residue_pnsq16: u32,
        price_converter: num::Converter,
    ) {
        self.entry_price = Self::effective_entry_price(
            self.r#type,
            price_pns,
            price_residue_pnsq16,
            price_converter,
        );
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
    pub(crate) fn update_premium_pnl(&mut self, instant: types::StateInstant, premium_pnl: D256) {
        self.premium_pnl = premium_pnl;
        self.instant = instant;
        self.funding_instant = instant;
    }

    pub(crate) fn apply_mark_price(&mut self, instant: types::StateInstant, mark_price: UD64) {
        let sign = if self.r#type.is_long() { D256::ONE } else { D256::ONE.neg() };
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
        let sign = if self.r#type.is_long() { D256::ONE.neg() } else { D256::ONE };
        self.premium_pnl += sign * payment_per_unit * self.size.resize().to_signed();
        self.instant = instant;
        self.funding_instant = instant;
        true
    }

    pub(crate) fn apply_maintenance_margin(
        &mut self,
        instant: types::StateInstant,
        maintenance_margin: UD64,
    ) {
        self.maintenance_margin_requirement =
            self.entry_price.resize() * self.size.resize() / maintenance_margin.resize();
        self.instant = instant;
    }

    /// Calculates effective entry price considering:
    /// - CEIL rounding for LONG positions
    /// - FLOOR rounding for SHORT positions
    /// - 16 bit rounding residue preserved by the smart contract separately
    ///   from PNS rounded price
    ///
    ///   LONG  (stored entry_price = ceil):  
    ///   = (entry_price - 1) + residue / Q    if residue > 0
    ///   = entry_price                        if residue == 0
    ///
    ///   SHORT (stored entry_price = floor):
    ///   = entry_price  + residue / Q
    ///
    ///   where Q = 2^16 = 65536
    pub(crate) fn effective_entry_price(
        position_type: PositionType,
        price_pns: U256,
        price_residue_pnsq16: u32,
        price_converter: num::Converter,
    ) -> UD64 {
        const Q: UD64 = udec64!(65536).with_rounding_mode(fastnum::decimal::RoundingMode::Floor);
        if price_residue_pnsq16 == 0 {
            price_converter.from_unsigned(price_pns)
        } else {
            let mut entry_price = UD64::from_u64(price_pns.to())
                .with_rounding_mode(fastnum::decimal::RoundingMode::Floor); // SC allocates 32 bits
            if position_type.is_long() && entry_price >= UD64::ONE {
                entry_price -= UD64::ONE;
            }
            (entry_price
                + UD64::from_u32(price_residue_pnsq16)
                    .with_rounding_mode(fastnum::decimal::RoundingMode::Floor)
                    / Q)
                / price_converter.scale()
        }
    }
}

impl PositionType {
    pub fn is_long(&self) -> bool { matches!(self, PositionType::Long) }

    pub fn is_short(&self) -> bool { matches!(self, PositionType::Short) }
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

impl std::fmt::Display for PositionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PositionType::Long => write!(f, "Long"),
            PositionType::Short => write!(f, "Short"),
        }
    }
}

#[cfg(feature = "display")]
impl tabled::Tabled for Position {
    const LENGTH: usize = 9;

    fn fields(&self) -> Vec<std::borrow::Cow<'_, str>> {
        use colored::Colorize;
        vec![
            self.perpetual_id().to_string().into(),
            if self.r#type.is_long() {
                self.r#type().to_string().green().to_string().into()
            } else {
                self.r#type().to_string().red().to_string().into()
            },
            self.entry_price().to_string().into(),
            self.size().to_string().into(),
            self.deposit().to_string().into(),
            if self.delta_pnl.is_negative() {
                self.delta_pnl.to_string().red().to_string().into()
            } else {
                self.delta_pnl.to_string().green().to_string().into()
            },
            if self.premium_pnl.is_negative() {
                self.premium_pnl.to_string().red().to_string().into()
            } else {
                self.premium_pnl.to_string().green().to_string().into()
            },
            if self.pnl().is_negative() {
                self.pnl().to_string().red().to_string().into()
            } else {
                self.pnl().to_string().green().to_string().into()
            },
            format!("{:.6}", self.liquidation_price()).into(),
            format!("{:.6}", self.bankruptcy_price()).into(),
        ]
    }

    fn headers() -> Vec<std::borrow::Cow<'static, str>> {
        vec![
            "Perp ID".into(),
            "Type".into(),
            "Entry Price".into(),
            "Size".into(),
            "Deposit".into(),
            "Delta PnL".into(),
            "Premium PnL".into(),
            "Total PnL".into(),
            "Liq Price".into(),
            "Bnkrp Price".into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use fastnum::{dec256, udec64, udec128};

    use super::*;
    use crate::types::StateInstant;

    #[test]
    fn test_effective_entry_price() {
        let pc = num::Converter::new(4);
        assert_eq!(
            Position::effective_entry_price(PositionType::Long, U256::from(1), 0, pc),
            udec64!(0.0001)
        );
        assert_eq!(
            Position::effective_entry_price(PositionType::Long, U256::from(1), 1, pc),
            udec64!(0.00000000152587890625) // 1 / 65536 * 10 ^ -4
        );
        assert_eq!(
            Position::effective_entry_price(PositionType::Long, U256::from(10001), 8, pc),
            udec64!(1.00000001220703125) // 1 + (8 / 65536 * 10 ^ -4)
        );
        assert_eq!(
            Position::effective_entry_price(PositionType::Long, U256::from(10000), 65535, pc),
            udec64!(0.9999999984741210937) // 1 - (1 / 65536 * 10 ^ -4)
        );

        assert_eq!(
            Position::effective_entry_price(PositionType::Short, U256::from(1), 0, pc),
            udec64!(0.0001)
        );
        assert_eq!(
            Position::effective_entry_price(PositionType::Short, U256::from(0), 1, pc),
            udec64!(0.00000000152587890625) // 1 / 65536 * 10 ^ -4
        );
        assert_eq!(
            Position::effective_entry_price(PositionType::Short, U256::from(10000), 8, pc),
            udec64!(1.00000001220703125) // 1 + (8 / 65536 * 10 ^ -4)
        );
        assert_eq!(
            Position::effective_entry_price(PositionType::Short, U256::from(9999), 65535, pc),
            udec64!(0.9999999984741210937) // 1 - (1 / 65536 * 10 ^ -4)
        );
    }

    #[test]
    fn test_apply_mark_price() {
        let pc = num::Converter::new(4);
        let mut pos = Position::opened(
            StateInstant::default(),
            1,
            1,
            PositionType::Long,
            U256::from(100),
            0,
            pc,
            udec64!(10),
            UD128::ZERO,
            UD64::ONE,
        );

        pos.apply_mark_price(StateInstant::default(), udec64!(0.015));
        assert_eq!(pos.delta_pnl(), dec256!(0.05));

        pos.apply_mark_price(StateInstant::default(), udec64!(0.005));
        assert_eq!(pos.delta_pnl(), dec256!(-0.05));

        let mut pos = Position::opened(
            StateInstant::default(),
            1,
            1,
            PositionType::Short,
            U256::from(100),
            0,
            pc,
            udec64!(10),
            UD128::ZERO,
            UD64::ONE,
        );
        pos.apply_mark_price(StateInstant::default(), udec64!(0.015));
        assert_eq!(pos.delta_pnl(), dec256!(-0.05));

        pos.apply_mark_price(StateInstant::default(), udec64!(0.005));
        assert_eq!(pos.delta_pnl(), dec256!(0.05));
    }

    #[test]
    fn test_apply_funding_payment() {
        let pc = num::Converter::new(4);
        let (i0, i1, i2) =
            (StateInstant::default(), StateInstant::new(1, 1), StateInstant::new(2, 2));
        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Long,
            U256::from(100),
            0,
            pc,
            udec64!(10),
            UD128::ZERO,
            UD64::ONE,
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
            U256::from(100),
            0,
            pc,
            udec64!(10),
            UD128::ZERO,
            UD64::ONE,
        );

        pos.apply_funding_payment(i1, dec256!(5));
        assert_eq!(pos.premium_pnl(), dec256!(50));

        pos.apply_funding_payment(i2, dec256!(-10));
        assert_eq!(pos.premium_pnl(), dec256!(-50));
    }

    #[test]
    fn test_maintenance_margin_requirement() {
        let pc = num::Converter::new(4);
        let i0 = StateInstant::default();
        let (mm1, mm2) = (udec64!(20), udec64!(10));

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Long,
            U256::from(1000000),
            0,
            pc,
            udec64!(10),
            udec128!(100),
            mm1,
        );
        assert_eq!(pos.maintenance_margin_requirement(), udec128!(50));

        pos.update_entry_price(i0, U256::from(800000), 0, pc);
        pos.apply_maintenance_margin(i0, mm1);
        assert_eq!(pos.maintenance_margin_requirement(), udec128!(40));

        pos.update_size(i0, udec64!(20));
        pos.apply_maintenance_margin(i0, mm1);
        assert_eq!(pos.maintenance_margin_requirement(), udec128!(80));

        pos.apply_maintenance_margin(i0, mm2);
        assert_eq!(pos.maintenance_margin_requirement(), udec128!(160));

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Short,
            U256::from(1000000),
            0,
            pc,
            udec64!(10),
            udec128!(100),
            mm1,
        );
        assert_eq!(pos.maintenance_margin_requirement(), udec128!(50));

        pos.update_entry_price(i0, U256::from(800000), 0, pc);
        pos.apply_maintenance_margin(i0, mm1);
        assert_eq!(pos.maintenance_margin_requirement(), udec128!(40));

        pos.update_size(i0, udec64!(20));
        pos.apply_maintenance_margin(i0, mm1);
        assert_eq!(pos.maintenance_margin_requirement(), udec128!(80));

        pos.apply_maintenance_margin(i0, mm2);
        assert_eq!(pos.maintenance_margin_requirement(), udec128!(160));
    }

    #[test]
    fn test_liquidation_price() {
        let pc = num::Converter::new(4);
        let (i0, i1) = (StateInstant::default(), StateInstant::new(1, 1));
        let mm1 = udec64!(20);

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Long,
            U256::from(1000000),
            0,
            pc,
            udec64!(10),
            udec128!(100),
            mm1,
        );
        assert_eq!(pos.liquidation_price(), udec64!(95));

        assert!(pos.apply_funding_payment(i1, dec256!(5)));
        assert_eq!(pos.liquidation_price(), udec64!(100));

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Short,
            U256::from(1000000),
            0,
            pc,
            udec64!(10),
            udec128!(100),
            mm1,
        );
        assert_eq!(pos.liquidation_price(), udec64!(105));

        assert!(pos.apply_funding_payment(i1, dec256!(-5)));
        assert_eq!(pos.liquidation_price(), udec64!(100));
    }

    #[test]
    fn test_bankruptcy_price() {
        let pc = num::Converter::new(4);
        let (i0, i1) = (StateInstant::default(), StateInstant::new(1, 1));
        let mm1 = udec64!(20);

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Long,
            U256::from(1000000),
            0,
            pc,
            udec64!(10),
            udec128!(100),
            mm1,
        );
        assert_eq!(pos.bankruptcy_price(), udec64!(90));

        assert!(pos.apply_funding_payment(i1, dec256!(5)));
        assert_eq!(pos.bankruptcy_price(), udec64!(95));

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Short,
            U256::from(1000000),
            0,
            pc,
            udec64!(10),
            udec128!(100),
            mm1,
        );
        assert_eq!(pos.bankruptcy_price(), udec64!(110));

        assert!(pos.apply_funding_payment(i1, dec256!(-5)));
        assert_eq!(pos.bankruptcy_price(), udec64!(105));
    }

    #[test]
    fn test_apply_mark_price_with_residue() {
        let i0 = StateInstant::default();
        let half_lsb = dec256!(0.0000005);
        let pc = num::Converter::new(6);

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Long,
            U256::from(100000000),
            32768, // residue = Q/2
            pc,
            udec64!(10),
            UD128::ZERO,
            UD64::ONE,
        );
        pos.apply_mark_price(i0, udec64!(100));
        // mark - effective = 100 - (100 - 0.5e-6) = +0.5e-6, * size 10 = 0.5e-5
        assert_eq!(pos.delta_pnl(), half_lsb * dec256!(10));

        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Short,
            U256::from(100000000),
            32768,
            pc,
            udec64!(10),
            UD128::ZERO,
            UD64::ONE,
        );
        pos.apply_mark_price(i0, udec64!(100));
        // short: sign flip, effective = entry + 0.5e-6, (mark - effective) = -0.5e-6
        // delta_pnl = -1 * -0.5e-6 * 10 = 0.5e-5
        assert_eq!(pos.delta_pnl(), half_lsb * dec256!(10));

        // residue = 0 reproduces legacy behavior byte-identically.
        let mut pos = Position::opened(
            i0,
            1,
            1,
            PositionType::Long,
            U256::from(100000000),
            0,
            pc,
            udec64!(10),
            UD128::ZERO,
            UD64::ONE,
        );
        pos.apply_mark_price(i0, udec64!(150));
        assert_eq!(pos.delta_pnl(), dec256!(500));
    }
}
