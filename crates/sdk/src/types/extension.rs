//! Builder attribution, client order IDs, and the versioned order-extension
//! envelope that carries both beside an order on the V2 entrypoints.

use alloy::{
    primitives::{Bytes, U256},
    sol_types::SolValue,
};
use fastnum::UD64;
use thiserror::Error;

use crate::num;

/// Envelope version carrying builder attribution alone
/// (`OrderExtensionLib._ORDER_EXT_VERSION_1`, contract v1.1.7.4+).
pub const ORDER_EXTENSION_VERSION_1: u16 = 1;

/// Envelope version carrying builder attribution and the two client order IDs
/// (`OrderExtensionLib._ORDER_EXT_VERSION_2`, contract v1.1.7.6+).
///
/// An *unknown* version to every earlier contract, which skips the order on
/// the batched and forwarded paths and reverts the single-order one - so it
/// is only ever encoded once the deployed contract is known to accept it.
pub const ORDER_EXTENSION_VERSION_2: u16 = 2;

/// Envelope version tag of a builder-only envelope. Retained for callers of
/// the first builder-code release; see [`ORDER_EXTENSION_VERSION_1`].
pub const ORDER_EXTENSION_VERSION: u16 = ORDER_EXTENSION_VERSION_1;

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

/// Failure encoding or decoding an order-extension envelope.
#[derive(Clone, Debug, Error)]
pub enum OrderExtensionError {
    /// Envelope is larger than the contract's decoder accepts. Structural
    /// fault - reverts on every path.
    #[error("order extension of {0} bytes exceeds maximum of {MAX_ORDER_EXTENSION_BYTES}")]
    ExceedsMaximumSize(usize),

    /// Envelope is not `abi.encode(uint16, bytes)`, or its payload is not the
    /// word tuple its version calls for. Structural fault - reverts on every
    /// path.
    #[error("malformed order extension envelope")]
    Malformed,

    /// Envelope version tag is not one the contract knows - which, before
    /// v1.1.7.6, includes [`ORDER_EXTENSION_VERSION_2`]. Recoverable fault -
    /// the contract skips just this order on the batched/forwarded paths.
    #[error("unsupported order extension version: {0}")]
    UnsupportedVersion(u16),

    /// Builder code is out of the `uint8` range. Recoverable fault.
    #[error("builder id {id} exceeds maximum of 255")]
    BuilderIdExceedsMaximum { id: U256 },

    /// Builder fee rate exceeds [`MAX_BUILDER_FEE_PER_100K`]. Recoverable
    /// fault.
    #[error("builder fee {0} exceeds maximum of {MAX_BUILDER_FEE_PER_100K} Per100K")]
    FeeExceedsMaximum(U256),

    /// A client order ID is out of the `uint32` range. Never truncated by the
    /// contract: a recoverable fault, like a builder field out of range.
    #[error("client order id {0} exceeds maximum of 4294967295")]
    ClientOrderIdExceedsMaximum(U256),
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

    /// Encodes a builder-only (version 1) envelope, rejecting a fee rate the
    /// contract's decoder would reject.
    ///
    /// The envelope every contract since v1.1.7.4 accepts; see
    /// [`OrderExtension::encode`] for one that also names client order IDs.
    pub fn encode(&self) -> Result<Bytes, OrderExtensionError> {
        OrderExtension::from(*self).encode()
    }

    /// Decodes the builder attribution out of an envelope of either version,
    /// under the current contract's rules.
    ///
    /// An empty envelope is the no-builder fast path and yields `None`, as
    /// does a version-2 envelope that names client order IDs without a
    /// builder. See [`OrderExtension::decode`].
    pub fn decode(extension: &[u8]) -> Result<Option<Self>, OrderExtensionError> {
        OrderExtension::decode(extension).map(|ext| ext.builder)
    }
}

impl std::fmt::Display for BuilderAttribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "🏗{}@{}", self.builder_id, self.fee)
    }
}

/// Everything a V2 entrypoint carries beside an order descriptor: builder
/// attribution and, from contract v1.1.7.6, the client order IDs.
///
/// Mirrors `OrderExtensionLib.decodeOrderExtension`:
///
/// ```text
/// extension = abi.encode(uint16 version, bytes payload)
/// version 1: payload = abi.encode(uint256 builderId, uint256 builderFeePer100K)
/// version 2: payload = abi.encode(uint256 builderId, uint256 builderFeePer100K,
///                                 uint256 clientOrderId, uint256 newClientOrderId)
/// ```
///
/// The empty extension is the contract's fast path, byte-identical to a V1
/// order. [`Self::encode`] picks the lowest version that carries the content,
/// so an order that names a builder and no ID keeps the envelope every
/// contract since v1.1.7.4 accepts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrderExtension {
    builder: Option<BuilderAttribution>,
    client_order_id: Option<super::ClientOrderId>,
    new_client_order_id: Option<super::ClientOrderId>,
}

impl OrderExtension {
    pub fn new(
        builder: Option<BuilderAttribution>,
        client_order_id: Option<super::ClientOrderId>,
        new_client_order_id: Option<super::ClientOrderId>,
    ) -> Self {
        Self { builder, client_order_id, new_client_order_id }
    }

    /// Builder the order is attributed to, if any.
    pub fn builder(&self) -> Option<BuilderAttribution> { self.builder }

    /// On an order that is placed: the ID to store with it. On a cancel or a
    /// change: the ID the order is expected to carry, the request being
    /// refused with `ClientOrderIdMismatch` if it carries another.
    pub fn client_order_id(&self) -> Option<super::ClientOrderId> { self.client_order_id }

    /// On a change only: the ID the order carries from then on. Ignored by
    /// every other operation.
    pub fn new_client_order_id(&self) -> Option<super::ClientOrderId> { self.new_client_order_id }

    /// Whether any client order ID is named, which is what calls for a
    /// version-2 envelope.
    pub fn has_client_order_ids(&self) -> bool {
        self.client_order_id.is_some() || self.new_client_order_id.is_some()
    }

    /// Whether this is the empty fast path, encoded as no envelope at all.
    pub fn is_empty(&self) -> bool { self.builder.is_none() && !self.has_client_order_ids() }

    /// Envelope version this content needs on the wire: `None` for the empty
    /// fast path, [`ORDER_EXTENSION_VERSION_1`] for a builder alone,
    /// [`ORDER_EXTENSION_VERSION_2`] once an ID is named.
    pub fn version(&self) -> Option<u16> {
        if self.has_client_order_ids() {
            Some(ORDER_EXTENSION_VERSION_2)
        } else if self.builder.is_some() {
            Some(ORDER_EXTENSION_VERSION_1)
        } else {
            None
        }
    }

    /// Encodes the envelope to submit with a V2 order entrypoint, rejecting a
    /// builder fee rate the contract's decoder would reject.
    ///
    /// The contract *reverts* on an out-of-range fee on the single-order path
    /// and skips the order (emitting `OrderExtensionRejected`) on the batched
    /// and forwarded paths, so an envelope is never worth building without the
    /// range check. The IDs are in range by type.
    ///
    /// Whether the deployed contract accepts the version this encodes is not
    /// checked here - see [`super::OrderRequest::prepare_v2`], which does.
    pub fn encode(&self) -> Result<Bytes, OrderExtensionError> {
        let (builder_id, fee_per_100k) = match self.builder {
            Some(builder) => {
                let fee_per_100k = builder.fee_per_100k();
                if fee_per_100k > U256::from(MAX_BUILDER_FEE_PER_100K) {
                    return Err(OrderExtensionError::FeeExceedsMaximum(fee_per_100k));
                }
                (U256::from(builder.builder_id), fee_per_100k)
            },
            None => (U256::ZERO, U256::ZERO),
        };
        let Some(version) = self.version() else {
            return Ok(Bytes::new());
        };
        let payload = if version == ORDER_EXTENSION_VERSION_2 {
            (
                builder_id,
                fee_per_100k,
                U256::from(self.client_order_id.map_or(0, |id| id.get())),
                U256::from(self.new_client_order_id.map_or(0, |id| id.get())),
            )
                .abi_encode_params()
        } else {
            (builder_id, fee_per_100k).abi_encode_params()
        };
        Ok((version, Bytes::from(payload)).abi_encode_params().into())
    }

    /// Decodes the envelope emitted with `OrderRequestV2` / `OrderRequestV3`,
    /// under the current contract's rules - both versions known.
    ///
    /// An empty envelope is the fast path and yields the default, empty
    /// extension.
    /// Rejections mirror the contract's decoder, so an envelope this returns an
    /// error for is one the contract itself rejected (see
    /// [`OrderExtensionError`] for which failures revert and which skip a
    /// single order).
    pub fn decode(extension: &[u8]) -> Result<Self, OrderExtensionError> {
        Self::decode_with(extension, true)
    }

    /// Decodes the envelope the way the contract that emitted it did:
    /// `client_order_ids` says whether that contract knew version 2
    /// (v1.1.7.6+), which the order-start event's own version tells -
    /// `OrderRequestV2` comes from contracts that did not, `OrderRequestV3`
    /// from contracts that do. Before it, a version-2 envelope is an unknown
    /// version - the order was skipped with no builder and no IDs - and
    /// replaying that block must report the rejection it was, not the IDs it
    /// carried.
    pub fn decode_with(
        extension: &[u8],
        client_order_ids: bool,
    ) -> Result<Self, OrderExtensionError> {
        if extension.is_empty() {
            return Ok(Self::default());
        }
        if extension.len() > MAX_ORDER_EXTENSION_BYTES {
            return Err(OrderExtensionError::ExceedsMaximumSize(extension.len()));
        }
        let (version, payload) = <(u16, Bytes)>::abi_decode_params(extension)
            .map_err(|_| OrderExtensionError::Malformed)?;
        let (builder_id, fee_per_100k, client_order_id, new_client_order_id) = match version {
            ORDER_EXTENSION_VERSION_1 => {
                let (builder_id, fee_per_100k) = <(U256, U256)>::abi_decode_params(&payload)
                    .map_err(|_| OrderExtensionError::Malformed)?;
                (builder_id, fee_per_100k, U256::ZERO, U256::ZERO)
            },
            ORDER_EXTENSION_VERSION_2 if client_order_ids => {
                <(U256, U256, U256, U256)>::abi_decode_params(&payload)
                    .map_err(|_| OrderExtensionError::Malformed)?
            },
            version => return Err(OrderExtensionError::UnsupportedVersion(version)),
        };
        // Checked in the contract's order: the IDs first, then the builder
        // fields, so the error names what the contract named
        for id in [client_order_id, new_client_order_id] {
            if id > U256::from(u32::MAX) {
                return Err(OrderExtensionError::ClientOrderIdExceedsMaximum(id));
            }
        }
        if builder_id > U256::from(u8::MAX) {
            return Err(OrderExtensionError::BuilderIdExceedsMaximum { id: builder_id });
        }
        if fee_per_100k > U256::from(MAX_BUILDER_FEE_PER_100K) {
            return Err(OrderExtensionError::FeeExceedsMaximum(fee_per_100k));
        }
        Ok(Self {
            // A zero builder code is "no builder" whatever fee rides with it,
            // which is also how a resting order reports it
            builder: (!builder_id.is_zero())
                .then(|| BuilderAttribution::from_raw(builder_id.to(), fee_per_100k)),
            client_order_id: super::ClientOrderId::new(client_order_id.to()),
            new_client_order_id: super::ClientOrderId::new(new_client_order_id.to()),
        })
    }
}

impl From<BuilderAttribution> for OrderExtension {
    fn from(builder: BuilderAttribution) -> Self {
        Self { builder: Some(builder), client_order_id: None, new_client_order_id: None }
    }
}

impl std::fmt::Display for OrderExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = vec![];
        if let Some(builder) = self.builder {
            parts.push(builder.to_string());
        }
        if let Some(id) = self.client_order_id {
            parts.push(format!("#{id}"));
        }
        if let Some(id) = self.new_client_order_id {
            parts.push(format!("→#{id}"));
        }
        if parts.is_empty() { write!(f, "-") } else { write!(f, "{}", parts.join(" ")) }
    }
}

#[cfg(test)]
mod tests {
    use fastnum::udec64;

    use super::*;

    fn coid(id: u32) -> super::super::ClientOrderId {
        super::super::ClientOrderId::new(id).expect("non-zero client order id")
    }

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
        assert_eq!(OrderExtension::decode(&encoded).unwrap(), OrderExtension::from(attribution));
        assert_eq!(OrderExtension::from(attribution).version(), Some(ORDER_EXTENSION_VERSION_1));
    }

    #[test]
    fn empty_envelope_is_no_builder() {
        assert_eq!(BuilderAttribution::decode(&[]).unwrap(), None);
        assert_eq!(OrderExtension::decode(&[]).unwrap(), OrderExtension::default());
        assert_eq!(OrderExtension::default().encode().unwrap(), Bytes::new());
        assert_eq!(OrderExtension::default().version(), None);
    }

    /// The worked example of the contract's own fact sheet
    /// (`CLIENT_ORDER_ID.md`, EXO-018): no builder, `clientOrderId =
    /// 0xA001`, `newClientOrderId = 0xA002`, seven words.
    #[test]
    fn client_order_id_envelope_matches_the_contract_fact_sheet() {
        let ext = OrderExtension::new(None, Some(coid(0xA001)), Some(coid(0xA002)));
        let encoded = ext.encode().expect("a valid envelope");
        assert_eq!(ext.version(), Some(ORDER_EXTENSION_VERSION_2));
        assert_eq!(encoded.len(), 224);
        let words: Vec<U256> = encoded.chunks(32).map(U256::from_be_slice).collect();
        assert_eq!(words, [2u64, 0x40, 0x80, 0, 0, 0xA001, 0xA002].map(U256::from), "{encoded}",);
        assert_eq!(OrderExtension::decode(&encoded).unwrap(), ext);
        // The builder half reads as none
        assert_eq!(BuilderAttribution::decode(&encoded).unwrap(), None);

        // ...and with a builder alongside, every member round-trips
        let builder = BuilderAttribution::new(7, udec64!(0.0025));
        let ext = OrderExtension::new(Some(builder), Some(coid(1)), None);
        let encoded = ext.encode().unwrap();
        assert_eq!(encoded.len(), 224);
        assert_eq!(OrderExtension::decode(&encoded).unwrap(), ext);
        assert_eq!(BuilderAttribution::decode(&encoded).unwrap(), Some(builder));
    }

    /// Before v1.1.7.6 a version-2 envelope is an unknown version, and that is
    /// what replaying such a block must report - the contract applied no ID.
    #[test]
    fn version_2_is_unknown_to_a_contract_before_client_order_ids() {
        let encoded = OrderExtension::new(None, Some(coid(5)), None)
            .encode()
            .unwrap();
        assert!(matches!(
            OrderExtension::decode_with(&encoded, false),
            Err(OrderExtensionError::UnsupportedVersion(2)),
        ));
        assert!(OrderExtension::decode_with(&encoded, true).is_ok());
        // A version-1 envelope is read either way
        let encoded = BuilderAttribution::new(1, udec64!(0)).encode().unwrap();
        assert!(OrderExtension::decode_with(&encoded, false).is_ok());
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
        let envelope: Bytes = (ORDER_EXTENSION_VERSION_1, Bytes::from(payload))
            .abi_encode_params()
            .into();
        assert!(matches!(
            BuilderAttribution::decode(&envelope),
            Err(OrderExtensionError::FeeExceedsMaximum(_)),
        ));
    }

    /// An ID above 32 bits is never truncated by the contract, and is checked
    /// before the builder fields are.
    #[test]
    fn rejects_an_out_of_range_client_order_id() {
        let payload =
            (U256::from(300), U256::ZERO, U256::from(1u64 << 32), U256::ZERO).abi_encode_params();
        let envelope: Bytes = (ORDER_EXTENSION_VERSION_2, Bytes::from(payload))
            .abi_encode_params()
            .into();
        assert!(matches!(
            OrderExtension::decode(&envelope),
            Err(OrderExtensionError::ClientOrderIdExceedsMaximum(_)),
        ));
        // A version-2 payload of two words is a structural fault
        let payload = (U256::from(1), U256::ZERO).abi_encode_params();
        let envelope: Bytes = (ORDER_EXTENSION_VERSION_2, Bytes::from(payload))
            .abi_encode_params()
            .into();
        assert!(matches!(OrderExtension::decode(&envelope), Err(OrderExtensionError::Malformed)));
    }

    #[test]
    fn rejects_unknown_version_and_malformed_envelope() {
        let payload = (U256::from(1), U256::from(10)).abi_encode_params();
        let envelope: Bytes = (3u16, Bytes::from(payload)).abi_encode_params().into();
        assert!(matches!(
            BuilderAttribution::decode(&envelope),
            Err(OrderExtensionError::UnsupportedVersion(3)),
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
