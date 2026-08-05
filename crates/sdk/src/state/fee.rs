//! Keyed, tiered trading fee schedules.
//!
//! Since v1.1.7.4 fees are not a per-perpetual scalar pair but a *keyed
//! schedule*: eight `(taker, maker)` rates indexed by an account's fee tier.
//! Which schedule applies to a fill is selected by the perpetual's
//! [`FeeScheduleKey`] (exchange-wide default / RWA default / the perpetual's
//! own custom schedule); within it, the account's
//! [`crate::state::Account::fee_tier`] picks the tier, tier 0 being the base
//! rate.
//!
//! Both sides are resolved at fill time from the perpetual's current key and
//! the account's current tier - never snapshotted at order placement - so a
//! schedule, key or tier change takes effect on the next fill.

use fastnum::UD64;

use super::*;

/// Number of fee tiers in a fee schedule.
pub const FEE_TIERS: usize = 8;

/// Raw key of the exchange-wide default schedule (`C._DEFAULT_FEE_KEY`).
const DEFAULT_FEE_KEY: u32 = 1021;

/// Raw key of the exchange-wide RWA default schedule
/// (`C._DEFAULT_RWA_FEE_KEY`).
const DEFAULT_RWA_FEE_KEY: u32 = 1022;

/// Selects the fee schedule a perpetual's fees are resolved from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeeScheduleKey {
    /// Exchange-wide default schedule, shared by every perpetual that has not
    /// been repointed. Kept up to date by `FeeScheduleSet`.
    Default,

    /// Exchange-wide default schedule for real-world assets. Kept up to date by
    /// `RwaFeeScheduleSet`.
    RwaDefault,

    /// The perpetual's own custom schedule, keyed by its ID. Kept up to date by
    /// `PerpFeeScheduleSet`.
    Custom(types::PerpetualId),
}

impl FeeScheduleKey {
    /// Interprets the raw on-chain schedule key.
    pub fn from_raw(key: U256) -> Self {
        match key.to::<u32>() {
            DEFAULT_FEE_KEY => Self::Default,
            DEFAULT_RWA_FEE_KEY => Self::RwaDefault,
            perp_id => Self::Custom(perp_id),
        }
    }

    /// Raw on-chain schedule key.
    pub fn to_raw(self) -> U256 {
        match self {
            Self::Default => U256::from(DEFAULT_FEE_KEY),
            Self::RwaDefault => U256::from(DEFAULT_RWA_FEE_KEY),
            Self::Custom(perp_id) => U256::from(perp_id),
        }
    }
}

impl std::fmt::Display for FeeScheduleKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::RwaDefault => write!(f, "rwa"),
            Self::Custom(perp_id) => write!(f, "custom #{perp_id}"),
        }
    }
}

/// Fee schedule: a `(taker, maker)` fee pair per fee tier, with the key
/// identifying which schedule it is.
///
/// Fees are fractions of the traded amount, converted from the on-chain
/// hundred-thousandths (`Per100K`) representation.
#[derive(Clone, Copy)]
pub struct FeeSchedule {
    key: FeeScheduleKey,
    tiered: bool,
    taker_fees: [UD64; FEE_TIERS],
    maker_fees: [UD64; FEE_TIERS],
}

impl FeeSchedule {
    /// Builds a schedule from the on-chain `Per100K` rates.
    pub(crate) fn new(
        key: FeeScheduleKey,
        taker_fees_per_100k: [U256; FEE_TIERS],
        maker_fees_per_100k: [U256; FEE_TIERS],
        fee_converter: num::Converter,
    ) -> Self {
        Self {
            key,
            tiered: true,
            taker_fees: taker_fees_per_100k.map(|fee| fee_converter.from_unsigned(fee)),
            maker_fees: maker_fees_per_100k.map(|fee| fee_converter.from_unsigned(fee)),
        }
    }

    /// Builds a flat schedule with the same base rates in every tier.
    ///
    /// Used where only the base rates are observable - the `ContractAdded` and
    /// the deprecated `MakerFeeUpdated`/`TakerFeeUpdated` events report the
    /// tier-0 rate only.
    pub(crate) fn flat(key: FeeScheduleKey, taker_fee: UD64, maker_fee: UD64) -> Self {
        Self {
            key,
            tiered: false,
            taker_fees: [taker_fee; FEE_TIERS],
            maker_fees: [maker_fee; FEE_TIERS],
        }
    }

    /// Schedule this perpetual/exchange resolves its fees from.
    pub fn key(&self) -> FeeScheduleKey { self.key }

    /// Whether the rates were reported per tier, rather than filled in from a
    /// base rate observed on its own.
    ///
    /// False against a pre-v1.1.7.4 deployment, which has no tiers to report:
    /// every tier then carries the base rate, and reading a discounted one back
    /// tells the caller nothing the base rate did not.
    pub fn is_tiered(&self) -> bool { self.tiered }

    /// Taker fee of the given tier.
    ///
    /// Out-of-range tiers (the contract bounds them to `0..8` on write) resolve
    /// to the base rate.
    pub fn taker_fee(&self, tier: types::FeeTier) -> UD64 {
        self.taker_fees
            .get(tier as usize)
            .copied()
            .unwrap_or(self.taker_fees[0])
    }

    /// Maker fee of the given tier.
    ///
    /// Out-of-range tiers (the contract bounds them to `0..8` on write) resolve
    /// to the base rate.
    pub fn maker_fee(&self, tier: types::FeeTier) -> UD64 {
        self.maker_fees
            .get(tier as usize)
            .copied()
            .unwrap_or(self.maker_fees[0])
    }

    /// Base (tier 0) taker fee.
    pub fn base_taker_fee(&self) -> UD64 { self.taker_fees[0] }

    /// Base (tier 0) maker fee.
    pub fn base_maker_fee(&self) -> UD64 { self.maker_fees[0] }

    /// Taker fee of every tier, base rate first.
    pub fn taker_fees(&self) -> &[UD64; FEE_TIERS] { &self.taker_fees }

    /// Maker fee of every tier, base rate first.
    pub fn maker_fees(&self) -> &[UD64; FEE_TIERS] { &self.maker_fees }

    /// Same rates under a different key, for a perpetual repointed by
    /// `PerpFeeKeySet`.
    pub(crate) fn with_key(&self, key: FeeScheduleKey) -> Self { Self { key, ..*self } }

    /// Overrides the base (tier 0) rates, leaving the discounted tiers intact.
    ///
    /// Only the deprecated `MakerFeeUpdated`/`TakerFeeUpdated` events (replayed
    /// from pre-v1.1.7.4 history) report a tier-0-only change.
    pub(crate) fn with_base_taker_fee(&self, taker_fee: UD64) -> Self {
        let mut taker_fees = self.taker_fees;
        taker_fees[0] = taker_fee;
        Self { taker_fees, ..*self }
    }

    /// Overrides the base (tier 0) rates, leaving the discounted tiers intact.
    pub(crate) fn with_base_maker_fee(&self, maker_fee: UD64) -> Self {
        let mut maker_fees = self.maker_fees;
        maker_fees[0] = maker_fee;
        Self { maker_fees, ..*self }
    }
}

impl std::fmt::Debug for FeeSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FeeSchedule {{ key: {}, tiers: [", self.key)?;
        for tier in 0..FEE_TIERS {
            write!(
                f,
                "{}{}/{}",
                if tier > 0 { " " } else { "" },
                self.taker_fees[tier],
                self.maker_fees[tier],
            )?;
        }
        write!(f, "] }}")
    }
}

impl std::fmt::Display for FeeSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            // Full schedule, tier by tier
            write!(f, "{}: ", self.key)?;
            for tier in 0..FEE_TIERS {
                write!(
                    f,
                    "{}{}/{}",
                    if tier > 0 { " | " } else { "" },
                    self.taker_fees[tier],
                    self.maker_fees[tier],
                )?;
            }
            Ok(())
        } else {
            // Base rates only
            write!(f, "{} / {} ({})", self.base_taker_fee(), self.base_maker_fee(), self.key)
        }
    }
}
