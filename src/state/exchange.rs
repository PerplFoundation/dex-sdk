use fastnum::UD256;
use hashbrown::HashMap;

use super::*;
use crate::Chain;

/// Exchange state snapshot.
///
/// [`super::SnapshotBuilder`] can be used to create the snapshot at
/// specified/latest block, which can then be kept up to date by
/// events from [`crate::stream::raw`].
#[derive(Clone, Debug)]
pub struct Exchange {
    chain: Chain,
    instant: types::StateInstant,
    collateral_converter: num::Converter,
    funding_interval_blocks: u32,
    min_post: UD256,
    min_settle: UD256,
    recycle_fee: UD256,
    perpetuals: HashMap<types::PerpetualId, Perpetual>,
    accounts: HashMap<types::AccountId, Account>,
    is_halted: bool,
}

impl Exchange {
    pub(crate) fn new(
        chain: Chain,
        instant: types::StateInstant,
        collateral_converter: num::Converter,
        funding_interval_blocks: u32,
        min_post: UD256,
        min_settle: UD256,
        recycle_fee: UD256,
        perpetuals: HashMap<types::PerpetualId, Perpetual>,
        accounts: HashMap<types::AccountId, Account>,
        is_halted: bool,
    ) -> Self {
        Self {
            chain,
            instant,
            collateral_converter,
            funding_interval_blocks,
            min_post,
            min_settle,
            recycle_fee,
            perpetuals,
            accounts,
            is_halted,
        }
    }

    /// Revision of the exchange smart contract the SDK targeted at.
    pub const fn revision() -> &'static str {
        crate::abi::DEX_REVISION
    }

    /// Chain the snapshot collected from.
    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// Instant the snapshot is consistent with or was last updated at.
    pub fn instant(&self) -> types::StateInstant {
        self.instant
    }

    /// Converter of fixed-point <-> decimal numbers for collateral token
    /// amounts.
    pub fn collateral_converter(&self) -> num::Converter {
        self.collateral_converter
    }

    /// Funding interval in blocks.
    ///
    /// Each perpetual contract has own [super::perpetual::Perpetual::funding_start_block]  this interval
    /// applied to.
    pub fn funding_interval_blocks(&self) -> u32 {
        self.funding_interval_blocks
    }

    /// Minimal amount in collateral token that can be posted to the book.
    pub fn min_post(&self) -> UD256 {
        self.min_post
    }

    /// Minimal amount in collateral token that can be settled.
    pub fn min_settle(&self) -> UD256 {
        self.min_settle
    }

    /// Amount in collateral token locked with each posted order to
    /// pay the account that cleans it up:
    /// * When cancelled/changed by the original poster -> the original poster
    /// * When filled -> the original poster
    /// * In all other cases -> the one that performed the recycling
    pub fn recycle_fee(&self) -> UD256 {
        self.recycle_fee
    }

    /// Perpetual contracts state tracked within the exchange, according to initial
    /// snapshot building configuration.
    pub fn perpetuals(&self) -> &HashMap<types::PerpetualId, Perpetual> {
        &self.perpetuals
    }

    /// Accounts state tracked within the exchange, according to initial
    /// snapshot building configuration.
    pub fn accounts(&self) -> &HashMap<types::AccountId, Account> {
        &self.accounts
    }

    /// Indicates if exchange is being halted.
    pub fn is_halted(&self) -> bool {
        self.is_halted
    }
}
