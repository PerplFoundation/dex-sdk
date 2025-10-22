//! Perpetual DEX SDK.
//!
//! # Overview
//!
//! Convenient in-memory cache of on-chain exchange state.
//!
//! Use [`state::SnapshotBuilder`] to capture initial state snapshot, then
//! [`stream::raw`] to catch up with the recent state and keep snapshot
//! up to date.
//! 
//! See `./tests` for examples.
//!
//! # Limitations/follow-ups
//!
//! * Funding events processing is to follow.
//!
//! * Current version relies on log polling to implement reliably continuous
//!     stream of events. Future versions could improve indexing latency by utilizing
//!     WebSocket subscriptions and/or Monad [`execution events`].
//!
//! * State tracking is supported only for existing accounts and perpetual contracts.
//! 
//! * Test coverage is far below reasonable.
//!
//! # Testing
//!
//! [`testing`] module provides a local testing environment with collateral
//! token and exchange smart contracts deployed.
//!
//!
//! [`execution events`]: https://docs.monad.xyz/execution-events/

pub mod abi;
pub mod error;
pub mod num;
pub mod state;
pub mod stream;
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
            collateral_token: address!("0xDcfCC5d088923a3Bb3b12CC9DfD34810EAe24248"),
            deployed_at_block: 41748298,
            exchange: address!("0xce806f7E961eA0A4bF11F3424444245f09ffF20E"),
            perpetuals: vec![16, 32, 48, 64],
        }
    }

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

    pub fn perpetuals(&self) -> &[types::PerpetualId] {
        &self.perpetuals
    }
}
