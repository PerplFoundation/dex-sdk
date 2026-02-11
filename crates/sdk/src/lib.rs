//! [`Perpl`] DEX SDK.
//!
//! # Overview
//!
//! Convenient in-memory cache of on-chain exchange state.
//!
//! Use [`state::SnapshotBuilder`] to capture initial state snapshot, then
//! [`stream::raw`] to catch up with the recent state and keep snapshot
//! up to date.
//!
//! Use [`types::OrderRequest`] to prepare order requests to send them with
//! [`crate::abi::dex::Exchange::ExchangeInstance::execOrders`].
//!
//! See `./tests` for examples.
//!
//! # Limitations/follow-ups
//!
//! * Funding events processing is to follow.
//!
//! * Current version relies on log polling to implement reliably continuous
//!   stream of events. Future versions could improve indexing latency by
//!   utilizing WebSocket subscriptions and/or Monad [`execution events`].
//!
//! * Test coverage is far below reasonable.
//!
//! # Features
//!
//! | Feature | Default | Description |
//! | --- | --- | --- |
//! | `display` | yes | Enables [`std::fmt::Display`] implementation for state types. |
//! | `testing` | yes | Enables [`testing`] module. |
//!
//! # Testing
//!
//! [`testing`] module provides a local testing environment with collateral
//! token and exchange smart contracts deployed.
//!
//!
//! [`Perpl`]: https://perpl.xyz
//! [`execution events`]: https://docs.monad.xyz/execution-events/

pub mod abi;
pub mod error;
pub mod num;
pub mod state;
pub mod stream;
#[cfg(feature = "testing")]
pub mod testing;
pub mod types;

use alloy::primitives::{Address, address};

#[derive(Clone, Debug)]
/// Chain the exchange is operating on.
pub struct Chain {
    chain_id: u64,
    collateral_token: Address,
    deployed_at_block: u64,
    exchange: Address,
    perpetuals: Vec<types::PerpetualId>,
}

impl Chain {
    pub fn testnet() -> Self {
        Self {
            chain_id: 10143,
            collateral_token: address!("0xa9012a055bd4e0eDfF8Ce09f960291C09D5322dC"),
            deployed_at_block: 62953,
            exchange: address!("0x1964C32f0bE608E7D29302AFF5E61268E72080cc"),
            perpetuals: vec![16, 32, 48, 64, 256],
        }
    }

    pub fn custom(
        chain_id: u64,
        collateral_token: Address,
        deployed_at_block: u64,
        exchange: Address,
        perpetuals: Vec<types::PerpetualId>,
    ) -> Self {
        Self { chain_id, collateral_token, deployed_at_block, exchange, perpetuals }
    }

    pub fn chain_id(&self) -> u64 { self.chain_id }

    pub fn collateral_token(&self) -> Address { self.collateral_token }

    pub fn deployed_at_block(&self) -> u64 { self.deployed_at_block }

    pub fn exchange(&self) -> Address { self.exchange }

    pub fn perpetuals(&self) -> &[types::PerpetualId] { &self.perpetuals }
}
