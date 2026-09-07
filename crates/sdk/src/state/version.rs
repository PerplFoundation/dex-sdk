//! Deployed contract version and the feature set derived from it.
//!
//! The exchange is an upgradeable proxy, so the deployed implementation can lag
//! behind the ABI the SDK is compiled against. Since v1.1.7.4 the contract
//! reports its own version via `getContractVersion()` and stamps
//! `ContractVersionSet` inside the upgrade transaction, which makes capability
//! detection authoritative rather than inferred: [`ContractFeatures::probe`]
//! reads the version once while building a snapshot, and
//! [`ContractFeatures::observe_version`] follows it in both directions from the
//! event stream.
//!
//! Older deployments have no version getter at all, so they are detected by
//! probing a selector added by the release in question.

use alloy::{eips::BlockId, primitives::U256, providers::Provider};

use crate::{abi::dex, num, types};

/// Version of the deployed exchange smart contract.
///
/// Renders as `v1.<major>.<minor>.<patch>` - the leading `v1` epoch is fixed
/// for this contract's lifetime and changes only with an entirely new contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContractVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl ContractVersion {
    /// First version exposing the V2 information getters (`getPerpetualInfoV2`,
    /// `getPositionV2`) and the corresponding V2 position events.
    ///
    /// Predates `getContractVersion`, so this version is never reported by the
    /// contract itself - it is only reached through selector probing.
    pub const V2_GETTERS: Self = Self { major: 1, minor: 7, patch: 3 };

    /// First version exposing keyed fee schedules, per-account fee tiers,
    /// builder attribution, the perpetual-existence bitmap - and
    /// `getContractVersion` itself.
    pub const BUILDER_CODES: Self = Self { major: 1, minor: 7, patch: 4 };

    /// First version whose stored fee-schedule rates are in millionths (ppm)
    /// rather than hundred-thousandths, and whose fills charge the schedule fee
    /// on EVERY position size change rather than on additions only.
    ///
    /// The two arrived in the same release and neither has a signal of its own,
    /// so they share the threshold.
    pub const PPM_FEE_UNIT: Self = Self { major: 1, minor: 7, patch: 5 };

    pub const fn new(major: u64, minor: u64, patch: u64) -> Self { Self { major, minor, patch } }

    pub const fn major(&self) -> u64 { self.major }

    pub const fn minor(&self) -> u64 { self.minor }

    pub const fn patch(&self) -> u64 { self.patch }
}

impl std::fmt::Display for ContractVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v1.{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Feature set of the deployed exchange smart contract.
///
/// Each flag guards a group of selectors/events introduced by a single release,
/// so the SDK can index and snapshot a contract that has not been upgraded to
/// the revision the SDK targets ([`crate::state::Exchange::revision`]).
#[derive(Clone, Copy, Debug)]
pub struct ContractFeatures {
    version: Option<ContractVersion>,
    v2_state_getters: bool,
    keyed_fee_schedules: bool,
    builder_attribution: bool,
    perpetual_discovery: bool,
    ppm_fee_unit: bool,
}

impl ContractFeatures {
    /// Everything the SDK targets, with no version reported. Useful as a
    /// default for locally deployed contracts built from the SDK's own ABI.
    pub fn current() -> Self {
        Self {
            version: None,
            v2_state_getters: true,
            keyed_fee_schedules: true,
            builder_attribution: true,
            perpetual_discovery: true,
            ppm_fee_unit: true,
        }
    }

    /// Feature set of a known contract version.
    pub fn of(version: ContractVersion) -> Self {
        let builder_codes = version >= ContractVersion::BUILDER_CODES;
        Self {
            version: Some(version),
            v2_state_getters: version >= ContractVersion::V2_GETTERS,
            keyed_fee_schedules: builder_codes,
            builder_attribution: builder_codes,
            perpetual_discovery: builder_codes,
            ppm_fee_unit: version >= ContractVersion::PPM_FEE_UNIT,
        }
    }

    /// Version reported by the contract, if it exposes `getContractVersion`
    /// (v1.1.7.4+).
    pub fn version(&self) -> Option<ContractVersion> { self.version }

    /// `getPerpetualInfoV2` / `getPositionV2` and the V2 position events
    /// (`fundingSumScalingExp`, `priceResiduePNSQ16`) are available.
    pub fn v2_state_getters(&self) -> bool { self.v2_state_getters }

    /// Keyed 8-tier fee schedules with per-account fee tiers are available
    /// (`getPerpFeeSchedule`, `getFeeScheduleById`, `getAccountFeeTier` and
    /// the `FeeScheduleSet` / `DefaultPerpFeeScheduleSet` /
    /// `DefaultRwaFeeScheduleSet` / `PerpFeeSchedIdSet` /
    /// `AccountFeeTierSet` events).
    pub fn keyed_fee_schedules(&self) -> bool { self.keyed_fee_schedules }

    /// Builder attribution is available (`execOrderV2` and friends,
    /// `getOrderV2` and the `OrderRequestV2` / `MakerOrderFilledV2` /
    /// `TakerOrderFilledV2` events).
    pub fn builder_attribution(&self) -> bool { self.builder_attribution }

    /// The perpetual-existence bitmap is available
    /// (`getPerpetualExistsBitmap`), so the set of listed perpetuals can be
    /// discovered on-chain instead of being configured.
    pub fn perpetual_discovery(&self) -> bool { self.perpetual_discovery }

    /// Stored fee-schedule rates are in millionths (ppm) rather than
    /// hundred-thousandths, and every position size change is charged the
    /// schedule fee - a close or decrease on the removed notional, netted from
    /// the exit proceeds, where earlier releases charged additions only.
    ///
    /// The per-order builder fee is NOT affected: it stays `Per100K` on the
    /// wire, in order storage and in every event at any version.
    pub fn ppm_fee_unit(&self) -> bool { self.ppm_fee_unit }

    /// Converter for the fee-SCHEDULE rates this deployment reports, resolving
    /// the v1.1.7.5 redenomination.
    ///
    /// An unknown version reads as the pre-upgrade unit, which is right for
    /// every contract old enough not to report one.
    pub fn fee_rate_converter(&self) -> num::Converter {
        if self.ppm_fee_unit { num::ppm_fee_converter() } else { num::fee_converter() }
    }

    /// Detects the feature set of the deployed contract at `block_id`.
    ///
    /// Reads `getContractVersion()`, which is authoritative on v1.1.7.4+ and
    /// absent before it - so a revert *proves* the contract predates every
    /// feature that release introduced. What a revert leaves open is whether
    /// the deployment is v1.1.7.3b or older, resolved by probing
    /// `getPerpetualInfoV2` against `probe_perpetual`; unlike `getPositionV2`,
    /// the perpetual getter does not validate account existence, so the probe
    /// distinguishes selector presence from state. With no perpetual to probe
    /// against the V2 getters are assumed present - see
    /// [`Self::probe_v2_state_getters`] to resolve that once one is known.
    pub(crate) async fn probe<P: Provider>(
        instance: &dex::Exchange::ExchangeInstance<P>,
        block_id: BlockId,
        probe_perpetual: Option<types::PerpetualId>,
    ) -> Self {
        if let Ok(v) = instance
            .getContractVersion()
            .block(block_id)
            .call()
            .await
            .map(|v| ContractVersion::new(v.major.to(), v.minor.to(), v.patch.to()))
        {
            return Self::of(v);
        }

        let mut features = Self {
            version: None,
            v2_state_getters: true,
            keyed_fee_schedules: false,
            builder_attribution: false,
            perpetual_discovery: false,
            ppm_fee_unit: false,
        };
        if let Some(perp_id) = probe_perpetual {
            features
                .probe_v2_state_getters(instance, block_id, perp_id)
                .await;
        }
        features
    }

    /// Resolves [`Self::v2_state_getters`] on an unversioned contract by
    /// probing `getPerpetualInfoV2` against a known perpetual.
    ///
    /// A no-op once the contract reports a version, which settles the question
    /// outright.
    pub(crate) async fn probe_v2_state_getters<P: Provider>(
        &mut self,
        instance: &dex::Exchange::ExchangeInstance<P>,
        block_id: BlockId,
        perp_id: types::PerpetualId,
    ) {
        if self.version.is_some() {
            return;
        }
        self.v2_state_getters = instance
            .getPerpetualInfoV2(U256::from(perp_id))
            .block(block_id)
            .call()
            .await
            .is_ok();
    }

    /// Folds a version reported by `ContractVersionSet` into the feature set.
    ///
    /// The signal is authoritative, so it is followed in both directions: a
    /// downgrade below a feature's threshold withdraws that feature.
    pub(crate) fn observe_version(&mut self, version: ContractVersion) {
        *self = Self::of(version);
    }
}

impl Default for ContractFeatures {
    fn default() -> Self { Self::current() }
}

impl std::fmt::Display for ContractFeatures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.version {
            Some(version) => write!(f, "{version}"),
            None => write!(
                f,
                "unversioned ({})",
                if self.v2_state_getters { "V2 getters" } else { "V0 getters" },
            ),
        }
    }
}
