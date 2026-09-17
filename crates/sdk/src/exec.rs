//! Building the exchange call that executes a batch of order requests.
//!
//! Every operation the exchange takes on an order - posting it, cancelling it,
//! changing it, topping up its collateral - is a [`types::OrderRequest`]
//! through the same entrypoint, so they all reach the chain through this one
//! function rather than one submit function each.
//!
//! What comes back is alloy's own [`RawCallBuilder`], which is where the SDK's
//! responsibility ends: simulating it, signing it and waiting on the receipt
//! are the caller's, and alloy already has the vocabulary for all three. The
//! sender is left unset for the caller's fillers to supply, so a client that
//! signs with a local key, a remote signer or a hardware wallet is served the
//! same way.

use alloy::{contract::RawCallBuilder, providers::Provider, sol_types::SolCall};

use crate::{abi::dex, state, types};

/// Call executing `requests` in order against `exchange`.
///
/// `revert_on_fail` reverts the whole transaction when one request fails,
/// rather than letting the exchange skip it and emit an error event.
///
/// Picks the entrypoint the deployed contract supports: `execOrdersV2` carries
/// the builder-attribution envelopes, and the V1 `execOrders` has nothing to
/// put them in, so a contract without V2 support cannot honour an attributed
/// order at all - which is the error [`types::OrderRequest::prepare_v2`]
/// returns.
pub fn orders_call<P: Provider>(
    exchange: &state::Exchange,
    provider: P,
    requests: &[types::OrderRequest],
    revert_on_fail: bool,
) -> Result<RawCallBuilder<P>, types::OrderRequestBuilderError> {
    let attributed = requests
        .iter()
        .any(|request| request.builder_attribution().is_some());
    let input = if attributed || exchange.features().builder_attribution() {
        let mut order_descs = Vec::with_capacity(requests.len());
        let mut extensions = Vec::with_capacity(requests.len());
        for request in requests {
            let (desc, extension) = request.prepare_v2(exchange)?;
            order_descs.push(desc);
            extensions.push(extension);
        }
        dex::Exchange::execOrdersV2Call {
            orderDescs: order_descs,
            revertOnFail: revert_on_fail,
            extensions,
        }
        .abi_encode()
    } else {
        dex::Exchange::execOrdersCall {
            orderDescs: requests
                .iter()
                .map(|request| request.prepare(exchange))
                .collect(),
            revertOnFail: revert_on_fail,
        }
        .abi_encode()
    };

    // Encoded rather than taken from `ExchangeInstance`'s own methods: those
    // borrow the provider, so what they return cannot outlive the instance,
    // and the two entrypoints decode to different types besides. Neither
    // returns anything a caller reads - what the exchange did with each order
    // is in its events - so a raw builder loses nothing
    Ok(RawCallBuilder::new_raw(provider, input.into()).to(exchange.chain().exchange()))
}
