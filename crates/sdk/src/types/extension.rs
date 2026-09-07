//! Builder attribution and the V2 order-extension envelope carrying it.

use alloy::{
    primitives::{Bytes, U256},
    sol_types::SolValue,
};
use fastnum::UD64;
use thiserror::Error;

use crate::num;

/// Envelope version tag understood by the contract's decoder
/// (`OrderExtensionLib._ORDER_EXT_VERSION_1`).
pub const ORDER_EXTENSION_VERSION: u16 = 1;

/// Hard byte cap the contract's decoder applies to a single envelope
/// (`OrderExtensionLib._MAX_ORDER_EXT_BYTES`).
///
/// Exceeding it is a *structural* fault: it reverts the whole call on every
/// path, including batched ones with `revertOnFail = false`.
pub const MAX_ORDER_EXTENSION_BYTES: usize = 256;

/// Highest per-order builder fee rate the contract accepts, in `Per100K`
/// (`C._MAX_FEE / C._FEE_UNIT_SCALE`, i.e. 1%).
///
/// The envelope's unit is `Per100K` at every contract version - the
/// builder-code wire format is deliberately untouched by the v1.1.7.5 fee
/// redenomination - but the ceiling moved with it: `C._MAX_FEE` kept its stored
/// value while the denominator behind it went from 1e5 to 1e6, so the same
/// number went from meaning 10% to meaning 1%.
///
/// Applied unconditionally, not per version: an envelope is encoded to be
/// submitted against the head of the chain, and a rate above 1% is rejected
/// there.
pub const MAX_BUILDER_FEE_PER_100K: u32 = 1_000;

/// Builder attribution of a single order: which builder submitted it and the
/// additive fee rate that builder charges on it.
///
/// The rate is what the *order* requests; what a builder actually earns is
/// reported per fill by the `builder_fee` of
/// [`crate::state::OrderEventType::Filled`].
///
/// A non-zero [`Self::builder_id`] does not imply a fee: attribution with a
/// zero fee is valid, and is what a client that wants attribution without a
/// charge submits.
#[derive(Clone, Copy, PartialEq, Eq, derive_more::Debug)]
pub struct BuilderAttribution {
    builder_id: super::BuilderId,
    #[debug("{fee}")]
    fee: UD64,
}

/// Failure decoding a V2 order-extension envelope.
#[derive(Clone, Debug, Error)]
pub enum OrderExtensionError {
    /// Envelope is larger than the contract's decoder accepts. Structural
    /// fault - reverts on every path.
    #[error("order extension of {0} bytes exceeds maximum of {MAX_ORDER_EXTENSION_BYTES}")]
    ExceedsMaximumSize(usize),

    /// Envelope is not `abi.encode(uint16, bytes)`, or its payload is not
    /// `abi.encode(uint256, uint256)`. Structural fault - reverts on every
    /// path.
    #[error("malformed order extension envelope")]
    Malformed,

    /// Envelope version tag is not [`ORDER_EXTENSION_VERSION`]. Recoverable
    /// fault - the contract skips just this order on the batched/forwarded
    /// paths.
    #[error("unsupported order extension version: {0}")]
    UnsupportedVersion(u16),

    /// Builder code is out of the `uint8` range. Recoverable fault.
    #[error("builder id {id} exceeds maximum of 255")]
    BuilderIdExceedsMaximum { id: U256 },

    /// Builder fee rate exceeds [`MAX_BUILDER_FEE_PER_100K`]. Recoverable
    /// fault.
    #[error("builder fee {0} exceeds maximum of {MAX_BUILDER_FEE_PER_100K} Per100K")]
    FeeExceedsMaximum(U256),
}

impl BuilderAttribution {
    /// Attribution of an order to `builder_id`, charging an additive `fee`
    /// fraction of the traded amount on the size the order adds.
    ///
    /// The fee is bounded by [`MAX_BUILDER_FEE_PER_100K`] and quantized to
    /// [`num::FEE_SCALE`] decimal places on encoding; use
    /// [`Self::encode`] to detect an out-of-range rate before submitting.
    pub fn new(builder_id: super::BuilderId, fee: UD64) -> Self { Self { builder_id, fee } }

    /// Builder attribution as recorded on-chain, from the raw `Per100K` fee
    /// rate.
    pub(crate) fn from_raw(builder_id: super::BuilderId, fee_per_100k: U256) -> Self {
        Self { builder_id, fee: num::fee_converter().from_unsigned(fee_per_100k) }
    }

    /// Builder code the order is attributed to. Zero means no builder, in which
    /// case no envelope is submitted at all.
    pub fn builder_id(&self) -> super::BuilderId { self.builder_id }

    /// Additive builder fee *rate* requested by the order, as a fraction of the
    /// traded amount.
    pub fn fee(&self) -> UD64 { self.fee }

    /// Raw `Per100K` fee rate as submitted on-chain.
    pub fn fee_per_100k(&self) -> U256 { num::fee_converter().to_unsigned(self.fee) }

    /// Encodes the envelope to submit with a V2 order entrypoint, rejecting a
    /// fee rate the contract's decoder would reject.
    ///
    /// Mirrors `OrderExtensionLib.decodeOrderExtension`:
    /// `abi.encode(uint16 version, bytes payload)` where the version-1 payload
    /// is `abi.encode(uint256 builderId, uint256 builderFeePer100K)`.
    ///
    /// The contract *reverts* on an out-of-range fee on the single-order path
    /// and skips the order (emitting `OrderExtensionRejected`) on the batched
    /// and forwarded paths, so an envelope is never worth building without the
    /// range check.
    pub fn encode(&self) -> Result<Bytes, OrderExtensionError> {
        let fee_per_100k = self.fee_per_100k();
        if fee_per_100k > U256::from(MAX_BUILDER_FEE_PER_100K) {
            return Err(OrderExtensionError::FeeExceedsMaximum(fee_per_100k));
        }
        let payload = (U256::from(self.builder_id), fee_per_100k).abi_encode_params();
        Ok((ORDER_EXTENSION_VERSION, Bytes::from(payload))
            .abi_encode_params()
            .into())
    }

    /// Decodes the envelope emitted with `OrderRequestV2`.
    ///
    /// An empty envelope is the no-builder fast path and yields `None`.
    /// Rejections mirror the contract's decoder, so an envelope this returns an
    /// error for is one the contract itself rejected (see
    /// [`OrderExtensionError`] for which failures revert and which skip a
    /// single order).
    pub fn decode(extension: &[u8]) -> Result<Option<Self>, OrderExtensionError> {
        if extension.is_empty() {
            return Ok(None);
        }
        if extension.len() > MAX_ORDER_EXTENSION_BYTES {
            return Err(OrderExtensionError::ExceedsMaximumSize(extension.len()));
        }
        let (version, payload) = <(u16, Bytes)>::abi_decode_params(extension)
            .map_err(|_| OrderExtensionError::Malformed)?;
        if version != ORDER_EXTENSION_VERSION {
            return Err(OrderExtensionError::UnsupportedVersion(version));
        }
        let (builder_id, fee_per_100k) = <(U256, U256)>::abi_decode_params(&payload)
            .map_err(|_| OrderExtensionError::Malformed)?;
        if builder_id > U256::from(u8::MAX) {
            return Err(OrderExtensionError::BuilderIdExceedsMaximum { id: builder_id });
        }
        if fee_per_100k > U256::from(MAX_BUILDER_FEE_PER_100K) {
            return Err(OrderExtensionError::FeeExceedsMaximum(fee_per_100k));
        }
        Ok(Some(Self::from_raw(builder_id.to(), fee_per_100k)))
    }
}

impl std::fmt::Display for BuilderAttribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "🏗{}@{}", self.builder_id, self.fee)
    }
}

#[cfg(test)]
mod tests {
    use fastnum::udec64;

    use super::*;

    #[test]
    fn builder_attribution_envelope_round_trip() {
        let attribution = BuilderAttribution::new(7, udec64!(0.001));
        let encoded = attribution.encode().expect("fee within range");

        // A version-1 envelope is exactly 160 bytes: version word, payload
        // offset, payload length, and the two payload words.
        assert_eq!(encoded.len(), 160);
        assert!(encoded.len() <= MAX_ORDER_EXTENSION_BYTES);
        assert_eq!(attribution.fee_per_100k(), U256::from(100));
        assert_eq!(BuilderAttribution::decode(&encoded).unwrap(), Some(attribution));
    }

    #[test]
    fn empty_envelope_is_no_builder() {
        assert_eq!(BuilderAttribution::decode(&[]).unwrap(), None);
    }

    #[test]
    fn rejects_out_of_range_fee() {
        // 11% - above the contract's 10% per-order cap.
        let attribution = BuilderAttribution::new(1, udec64!(0.11));
        assert!(matches!(attribution.encode(), Err(OrderExtensionError::FeeExceedsMaximum(_)),));

        // ...and so is such an envelope on the way back in. It cannot be built
        // with `encode`, only received from a third-party submitter, so it is
        // hand-rolled here.
        let payload = (U256::from(1), U256::from(11_000)).abi_encode_params();
        let envelope: Bytes = (ORDER_EXTENSION_VERSION, Bytes::from(payload))
            .abi_encode_params()
            .into();
        assert!(matches!(
            BuilderAttribution::decode(&envelope),
            Err(OrderExtensionError::FeeExceedsMaximum(_)),
        ));
    }

    #[test]
    fn rejects_unknown_version_and_malformed_envelope() {
        let payload = (U256::from(1), U256::from(10)).abi_encode_params();
        let envelope: Bytes = (2u16, Bytes::from(payload)).abi_encode_params().into();
        assert!(matches!(
            BuilderAttribution::decode(&envelope),
            Err(OrderExtensionError::UnsupportedVersion(2)),
        ));

        assert!(matches!(
            BuilderAttribution::decode(&[0xffu8; 3]),
            Err(OrderExtensionError::Malformed),
        ));
        assert!(matches!(
            BuilderAttribution::decode(&[0u8; MAX_ORDER_EXTENSION_BYTES + 1]),
            Err(OrderExtensionError::ExceedsMaximumSize(_)),
        ));
    }
}
