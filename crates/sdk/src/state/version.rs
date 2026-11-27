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

    /// First version storing a client order ID with every resting order,
    /// carried by version 2 of the order-extension envelope and checked by a
    /// cancel or change that names one (`ClientOrderIdMismatch`); opening
    /// every order with `OrderRequestV3`, which adds the caller; accepting
    /// the decrease-collateral request and its cancellation as order types
    /// (`RequestDecreasePositionCollateral` /
    /// `CancelDecreaseCollateralRequest`, out of the enum range of every
    /// earlier contract, which reverts the WHOLE batch on them); keeping an
    /// operating maintenance margin fraction per perpetual; and capping an
    /// account at 150 resting orders per perpetual.
    ///
    /// Shipped together and gated together: `OrderDescEnum` gained its two
    /// members in the release that taught the envelope decoder version 2.
    pub const CLIENT_ORDER_IDS: Self = Self { major: 1, minor: 7, patch: 6 };

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
    client_order_ids: bool,
    decrease_collateral_orders: bool,
    operating_maintenance_margin: bool,
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
            client_order_ids: true,
            decrease_collateral_orders: true,
            operating_maintenance_margin: true,
        }
    }

    /// Feature set of a known contract version.
    pub fn of(version: ContractVersion) -> Self {
        let builder_codes = version >= ContractVersion::BUILDER_CODES;
        let client_order_ids = version >= ContractVersion::CLIENT_ORDER_IDS;
        Self {
            version: Some(version),
            v2_state_getters: version >= ContractVersion::V2_GETTERS,
            keyed_fee_schedules: builder_codes,
            builder_attribution: builder_codes,
            perpetual_discovery: builder_codes,
            ppm_fee_unit: version >= ContractVersion::PPM_FEE_UNIT,
            client_order_ids,
            decrease_collateral_orders: client_order_ids,
            operating_maintenance_margin: client_order_ids,
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

    /// Client order IDs are available: version 2 of the order-extension
    /// envelope is accepted, every resting order stores the ID it was placed
    /// with (`getOrderLocksV2`), a cancel or change naming one is checked
    /// against it (`ClientOrderIdMismatch`), and every order opens with
    /// `OrderRequestV3`.
    ///
    /// To an earlier contract a version-2 envelope is an unknown version -
    /// the order is skipped, or the single-order call reverts - so a request
    /// naming an ID is refused by [`crate::types::OrderRequestBuilder::build`]
    /// and [`crate::types::OrderRequest::prepare_v2`] until the upgrade is
    /// observed.
    pub fn client_order_ids(&self) -> bool { self.client_order_ids }

    /// The decrease-collateral request and its cancellation are order types
    /// ([`crate::types::RequestType::RequestDecreasePositionCollateral`] /
    /// [`crate::types::RequestType::CancelDecreaseCollateralRequest`]).
    ///
    /// Out of the enum range of every earlier contract, where the ABI decoder
    /// panics and the WHOLE call reverts - batches with `revertOnFail = false`
    /// included - so a request of either type is refused until the upgrade
    /// is observed, rather than let it take a batch down with it.
    pub fn decrease_collateral_orders(&self) -> bool { self.decrease_collateral_orders }

    /// Each perpetual keeps an operating maintenance margin fraction
    /// (`getOperatingMaintMarginFraction`,
    /// `OperatingMaintMarginFractionUpdated`) beside the contractual one -
    /// see [`crate::state::Perpetual::operating_maintenance_margin`].
    pub fn operating_maintenance_margin(&self) -> bool { self.operating_maintenance_margin }

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
            client_order_ids: false,
            decrease_collateral_orders: false,
            operating_maintenance_margin: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The v1.1.7.6 boundary decides whether a version-2 envelope is read and
    /// whether the decrease-collateral order types may be sent. Unknown is
    /// below it, like every other gate here.
    #[test]
    fn client_order_ids_threshold() {
        for (version, want) in [
            (ContractVersion::V2_GETTERS, false),
            (ContractVersion::BUILDER_CODES, false),
            (ContractVersion::PPM_FEE_UNIT, false), // the last release without them
            (ContractVersion::CLIENT_ORDER_IDS, true), // exactly the threshold
            (ContractVersion::new(8, 0, 0), true),
        ] {
            let features = ContractFeatures::of(version);
            assert_eq!(features.client_order_ids(), want, "{version}");
            assert_eq!(features.decrease_collateral_orders(), want, "{version}");
            assert_eq!(features.operating_maintenance_margin(), want, "{version}");
            // Nothing below the threshold moved
            assert_eq!(features.ppm_fee_unit(), version >= ContractVersion::PPM_FEE_UNIT);
            assert_eq!(features.builder_attribution(), version >= ContractVersion::BUILDER_CODES);
        }
        assert!(ContractFeatures::current().client_order_ids());
    }

    /// The signal is followed in both directions: a rollback below the
    /// threshold withdraws the features, as the forwarder would otherwise
    /// send what the rolled-back contract reverts on.
    #[test]
    fn version_observed_in_both_directions() {
        let mut features = ContractFeatures::of(ContractVersion::PPM_FEE_UNIT);
        features.observe_version(ContractVersion::CLIENT_ORDER_IDS);
        assert!(features.client_order_ids());
        features.observe_version(ContractVersion::PPM_FEE_UNIT);
        assert!(!features.client_order_ids());
        assert_eq!(features.version(), Some(ContractVersion::PPM_FEE_UNIT));
        assert_eq!(ContractVersion::CLIENT_ORDER_IDS.to_string(), "v1.1.7.6");
    }
}
