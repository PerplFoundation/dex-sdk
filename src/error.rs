use std::fmt::Display;

use alloy::{
    contract,
    primitives::Bytes,
    providers::{MulticallError, PendingTransactionError},
    sol_types::{self, SolInterface},
    transports,
};

use crate::{abi::errors::Exchange::ExchangeErrors, state::{OrderBookError, OrderParseError}, types};

pub type DexError = ProviderError<ExchangeErrors>;

/// Call/transaction revert reason decoded by
/// the provided known ABI or in a generic raw form
/// if can not be decoded.
#[derive(Debug)]
pub enum RevertReason<R> {
    Known(R),
    Generic(String),
    Unknown,
}

/// Error returned by the RPC provider as a result of call or
/// transaction execution.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError<R> {
    #[error("fatal error: {0}")]
    Fatal(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("unexpected empty RPC response")]
    NullResp,

    #[error("transaction ran out of gas")]
    OutOfGas,

    #[error("transaction reverted: {0:?}")]
    Reverted(Box<RevertReason<R>>),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("transaction timed out")]
    Timeout,

    #[error("block out of order, expected: {0}, got: {1}")]
    BlockOutOfOrder(u64, u64),

    #[error("order context expected, tx: {0}, log: {1}")]
    OrderContextExpected(u64, u64),

    #[error("order not found: {0}")]
    OrderNotFound(types::PerpetualId, types::OrderId),

    #[error("position not found, acc: {0}, perp: {1}")]
    PositionNotFound(types::AccountId, types::PerpetualId),

    #[error("order book error: {0}")]
    OrderBook(#[from] OrderBookError),

    #[error("order parse error: {0}")]
    OrderParse(#[from] OrderParseError),
}

impl<R: SolInterface> From<contract::Error> for ProviderError<R> {
    fn from(value: contract::Error) -> Self {
        match value {
            contract::Error::UnknownFunction(_) => Self::Fatal(value.to_string()),
            contract::Error::UnknownSelector(_) => Self::Fatal(value.to_string()),
            contract::Error::NotADeploymentTransaction => Self::Fatal(value.to_string()),
            contract::Error::ContractNotDeployed => Self::Fatal(value.to_string()),
            contract::Error::ZeroData(_, _) => Self::Fatal(value.to_string()),
            contract::Error::AbiError(_) => Self::Fatal(value.to_string()),
            contract::Error::TransportError(rpc_err) => Self::from(rpc_err),
            contract::Error::PendingTransactionError(err) => err.into(),
        }
    }
}

impl<R: SolInterface> From<PendingTransactionError> for ProviderError<R> {
    fn from(value: PendingTransactionError) -> Self {
        match value {
            alloy::providers::PendingTransactionError::FailedToRegister => {
                Self::Fatal(value.to_string())
            }
            alloy::providers::PendingTransactionError::TransportError(rpc_err) => {
                Self::from(rpc_err)
            }
            alloy::providers::PendingTransactionError::Recv(_) => {
                Self::Transport(value.to_string())
            }
            alloy::providers::PendingTransactionError::TxWatcher(err) => match err {
                alloy::providers::WatchTxError::Timeout => Self::Timeout,
            },
        }
    }
}

impl<E: Display, R: SolInterface> From<transports::RpcError<E>> for ProviderError<R> {
    fn from(value: transports::RpcError<E>) -> Self {
        match value {
            transports::RpcError::ErrorResp(ref resp) => {
                // Heuristic to determine if eth_call failed due to OutOfGas or
                // if transaction was reverted during the gas estimation
                let msg = resp.message.to_ascii_lowercase();
                if (resp.code == -32603) && (msg.contains("gas") || msg.contains("oog")) {
                    Self::OutOfGas
                } else if ((resp.code == -32600 || resp.code == -32601 || resp.code == -32602)
                    && (msg.contains("invalid") || msg.contains("not found")))
                    || (resp.code == -32603
                        && (msg.contains("block by number") || msg.contains("getting block")))
                {
                    Self::InvalidRequest(msg)
                } else if resp.code == 3 && msg.contains("reverted") {
                    Self::Reverted(Box::new(RevertReason::from(value)))
                } else {
                    Self::Transport(value.to_string())
                }
            }
            transports::RpcError::NullResp => Self::NullResp,
            _ => Self::Transport(value.to_string()),
        }
    }
}

impl<R: SolInterface> From<sol_types::Error> for ProviderError<R> {
    fn from(value: sol_types::Error) -> Self {
        Self::Fatal(value.to_string())
    }
}

impl<R: SolInterface> From<MulticallError> for ProviderError<R> {
    fn from(value: MulticallError) -> Self {
        match value {
            MulticallError::ValueTx => Self::InvalidRequest(value.to_string()),
            MulticallError::DecodeError(_) => Self::Fatal(value.to_string()),
            MulticallError::NoReturnData => Self::NullResp,
            MulticallError::CallFailed(bytes) => {
                Self::Reverted(Box::new(RevertReason::from(bytes)))
            }
            MulticallError::TransportError(rpc_err) => Self::from(rpc_err),
        }
    }
}

impl<E: Display, R: SolInterface> From<transports::RpcError<E>> for RevertReason<R> {
    fn from(value: transports::RpcError<E>) -> Self {
        match value.as_error_resp() {
            Some(payload) => match payload.as_decoded_interface_error::<R>() {
                Some(known) => Self::Known(known),
                None => Self::Generic(value.to_string()),
            },
            None => Self::Generic(value.to_string()),
        }
    }
}

impl<R: SolInterface> From<Bytes> for RevertReason<R> {
    fn from(value: Bytes) -> Self {
        match R::abi_decode(&value) {
            Ok(known) => Self::Known(known),
            Err(_) => Self::Generic(value.to_string()),
        }
    }
}
