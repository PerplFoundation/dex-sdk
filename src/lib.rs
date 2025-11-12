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
//! Use [`types::OrderRequest`] to prepare order requests to send them with
//! [`crate::abi::dex::Exchange::ExchangeInstance::execOpsAndOrders`].
//!
//! See `./tests` for examples.
//!
//! # Limitations/follow-ups
//!
//! * Funding events processing is to follow.
//!
//! * Current version relies on log polling to implement reliably continuous
//!   stream of events. Future versions could improve indexing latency by utilizing
//!   WebSocket subscriptions and/or Monad [`execution events`].
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
            collateral_token: address!("0x14e5777257991D719Fb750080cfCA49510A9F3f1"),
            deployed_at_block: 48858152,
            exchange: address!("0x5593AF79e5A0F20e623EF1B261adc9a05D5dA5E7"),
            perpetuals: vec![16, 32, 48],
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
