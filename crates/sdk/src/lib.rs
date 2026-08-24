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
//! [`crate::abi::dex::Exchange::ExchangeInstance::execOrdersV2`].
//!
//! The deployed contract may lag behind the revision the SDK targets, so the
//! snapshot detects the contract's [`state::ContractFeatures`] and degrades
//! gracefully rather than failing on a missing selector.
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
#[cfg(test)]
mod tests;
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
    excluded_perpetuals: Vec<types::PerpetualId>,
}

impl Chain {
    pub fn mainnet() -> Self {
        Self {
            chain_id: 143,
            collateral_token: address!("0x00000000eFE302BEAA2b3e6e1b18d08D69a9012a"),
            deployed_at_block: 54773010,
            exchange: address!("0x34B6552d57a35a1D042CcAe1951BD1C370112a6F"),
            perpetuals: vec![],
            excluded_perpetuals: vec![30],
        }
    }

    pub fn testnet() -> Self {
        Self {
            chain_id: 10143,
            collateral_token: address!("0xa9012a055bd4e0eDfF8Ce09f960291C09D5322dC"),
            deployed_at_block: 62953,
            exchange: address!("0x1964C32f0bE608E7D29302AFF5E61268E72080cc"),
            perpetuals: vec![],
            excluded_perpetuals: vec![],
        }
    }

    /// Chain the exchange is operating on, with the perpetual contracts to
    /// track.
    ///
    /// An empty `perpetuals` list means *every* perpetual listed on the
    /// exchange, discovered on-chain - see [`Chain::perpetuals`].
    pub fn custom(
        chain_id: u64,
        collateral_token: Address,
        deployed_at_block: u64,
        exchange: Address,
        perpetuals: Vec<types::PerpetualId>,
    ) -> Self {
        Self {
            chain_id,
            collateral_token,
            deployed_at_block,
            exchange,
            perpetuals,
            excluded_perpetuals: vec![],
        }
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn collateral_token(&self) -> Address {
        self.collateral_token
    }

    pub fn deployed_at_block(&self) -> u64 {
        self.deployed_at_block
    }

    pub fn exchange(&self) -> Address {
        self.exchange
    }

    /// Perpetual contracts to track, empty (the default) meaning every
    /// perpetual listed on the exchange.
    ///
    /// The exchange reports the set of listed contracts on-chain, so the SDK
    /// does not need to be told: an empty list makes
    /// [`state::SnapshotBuilder`] discover them at snapshot time. Configure it
    /// explicitly only to deliberately track a *subset*.
    pub fn perpetuals(&self) -> &[types::PerpetualId] {
        &self.perpetuals
    }

    /// Same chain, tracking only the given subset of perpetual contracts.
    pub fn with_perpetuals(mut self, perpetuals: Vec<types::PerpetualId>) -> Self {
        self.perpetuals = perpetuals;
        self
    }

    /// Perpetual contracts to leave out of on-chain discovery.
    ///
    /// Applies to discovery *only*: an explicitly configured
    /// [`Chain::perpetuals`] list is taken as given, exclusions and all, since
    /// naming a contract is a clearer statement of intent than the default set
    /// it would otherwise be filtered out of.
    pub fn excluded_perpetuals(&self) -> &[types::PerpetualId] {
        &self.excluded_perpetuals
    }

    /// Same chain, skipping the given perpetual contracts when discovering the
    /// set to track - see [`Chain::excluded_perpetuals`].
    ///
    /// Replaces the chain's default exclusions rather than adding to them, so
    /// passing an empty list discovers everything the exchange lists.
    pub fn with_excluded_perpetuals(mut self, perpetuals: Vec<types::PerpetualId>) -> Self {
        self.excluded_perpetuals = perpetuals;
        self
    }
}
