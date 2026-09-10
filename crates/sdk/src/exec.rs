//! Executing exchange transactions: build, simulate, send, wait.
//!
//! [`Call`] is the whole of it. It holds a transaction that has not been sent
//! yet and stages the steps separately, so a caller that wants to print the
//! calldata, ask an operator, or stop after the simulation can, while one that
//! wants none of that calls [`Call::submit`] and gets a receipt.
//!
//! The staging is deliberate: every operation the exchange takes on an order -
//! posting it, cancelling it, changing it, topping up its collateral - is a
//! [`types::OrderRequest`] through the same entrypoint, so they all reach the
//! chain through this one path rather than one submit function each.

use alloy::{
    network::{Ethereum, TransactionBuilder},
    primitives::{Address, Bytes, TxHash},
    providers::{PendingTransactionBuilder, Provider},
    rpc::types::{TransactionReceipt, TransactionRequest},
    sol_types::SolCall,
};

use crate::{abi::dex, error::DexError, state, types};

/// Transaction executing `requests` in order, sent by `from`.
///
/// `revert_on_fail` reverts the whole transaction when one request fails,
/// rather than letting the exchange skip it and emit an error event.
///
/// Picks the entrypoint the deployed contract supports: `execOrdersV2` carries
/// the builder-attribution envelopes, and the V1 `execOrders` has nothing to
/// put them in, so a contract without V2 support cannot honour an attributed
/// order at all - which is the error [`types::OrderRequest::prepare_v2`]
/// returns.
pub fn orders_transaction(
    exchange: &state::Exchange,
    requests: &[types::OrderRequest],
    revert_on_fail: bool,
    from: Address,
) -> Result<TransactionRequest, DexError> {
    if let Some(untracked) = requests
        .iter()
        .map(types::OrderRequest::perp_id)
        .find(|perp_id| !exchange.perpetuals().contains_key(perp_id))
    {
        return Err(DexError::PerpetualNotTracked(untracked));
    }

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

    Ok(TransactionRequest::default()
        .with_to(exchange.chain().exchange())
        .with_from(from)
        .with_input(Bytes::from(input)))
}

/// The [`Call`] executing `requests` - see [`orders_transaction`].
pub fn orders_call<P: Provider>(
    exchange: &state::Exchange,
    provider: P,
    from: Address,
    requests: &[types::OrderRequest],
    revert_on_fail: bool,
) -> Result<Call<P>, DexError> {
    Ok(Call::new(provider, orders_transaction(exchange, requests, revert_on_fail, from)?))
}

/// An exchange transaction that has not been sent, staged so a caller can act
/// between the steps.
///
/// [`Call::simulate`] proves the call would not revert against current state,
/// [`Call::send`] signs it and puts it on the wire, and [`Sent::wait`] waits
/// for the receipt. [`Call::submit`] is all three for callers that need
/// nothing in between.
#[derive(Clone, Debug)]
pub struct Call<P> {
    provider: P,
    tx: TransactionRequest,
}

impl<P: Provider> Call<P> {
    /// Wraps `tx` to be sent through `provider`.
    ///
    /// Sending needs a `provider` carrying the wallet that signs for the
    /// transaction's sender - see
    /// [`alloy::providers::ProviderBuilder::wallet`]. Stacking that wallet on
    /// top of an existing provider keeps whatever throttling, retry and poll
    /// settings it was built with, rather than dialling a second connection.
    /// Simulating needs no wallet at all.
    pub fn new(provider: P, tx: TransactionRequest) -> Self { Self { provider, tx } }

    /// Same call with an explicit gas limit, skipping estimation.
    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.tx.set_gas_limit(gas_limit);
        self
    }

    /// The transaction as it will be sent, for a caller that wants to show the
    /// calldata rather than submit it.
    pub fn transaction(&self) -> &TransactionRequest { &self.tx }

    /// Runs the call against current state without sending it, returning what
    /// the entrypoint returns.
    ///
    /// A revert here is the common outcome of a bad order - insufficient
    /// collateral, a stale mark, a reduce-only order with nothing to reduce -
    /// and comes back decoded to the contract's own error rather than a blob.
    pub async fn simulate(&self) -> Result<Bytes, DexError> {
        self.provider
            .call(self.tx.clone())
            .await
            .map_err(|err| DexError::Provider(err.into()))
    }

    /// Signs and sends, returning as soon as the node accepts the transaction.
    /// The receipt is [`Sent::wait`].
    pub async fn send(self) -> Result<Sent, DexError> {
        let pending = self
            .provider
            .send_transaction(self.tx)
            .await
            .map_err(|err| DexError::Provider(err.into()))?;
        Ok(Sent { hash: *pending.tx_hash(), pending })
    }

    /// Simulates, sends, and waits for a successful receipt.
    pub async fn submit(self) -> Result<TransactionReceipt, DexError> {
        self.simulate().await?;
        self.send().await?.wait().await
    }
}

/// A sent transaction, identified by its hash, whose receipt has not been
/// waited for yet.
#[derive(Debug)]
pub struct Sent {
    hash: TxHash,
    pending: PendingTransactionBuilder<Ethereum>,
}

impl Sent {
    pub fn tx_hash(&self) -> TxHash { self.hash }

    /// Waits for the receipt, treating a reverted status as an error.
    ///
    /// A successful receipt only says the transaction executed: what the
    /// exchange actually did with each order - accepted, filled, rejected - is
    /// in its events, which [`crate::state::Exchange::apply_events`] reads.
    pub async fn wait(self) -> Result<TransactionReceipt, DexError> {
        let receipt = self
            .pending
            .get_receipt()
            .await
            .map_err(|err| DexError::Provider(err.into()))?;
        if !receipt.status() {
            return Err(DexError::TransactionReverted(self.hash));
        }
        Ok(receipt)
    }
}
