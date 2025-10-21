use alloy::primitives::U256;
use fastnum::UD64;
use hashbrown::HashMap;

use super::*;
use crate::{abi::dex::Exchange::PerpetualInfo, types};

const FEE_SCALE: u8 = 5;
const LEVERAGE_SCALE: u8 = 2;

/// Perpetual contract tradeable at the exchange.
///
/// Provides the current state of contract parameters, market data and
/// order book.
#[derive(Clone, Debug)]
pub struct Perpetual {
    instant: types::StateInstant,
    id: types::PerpetualId,
    name: String,
    symbol: String,
    is_paused: bool,

    price_converter: num::Converter,
    size_converter: num::Converter,
    leverage_converter: num::Converter,
    base_price: UD64, // SC allocates 32 bits

    maker_fee: UD64,          // SC allocates 16 bits
    taker_fee: UD64,          // SC allocates 16 bits
    initial_margin: UD64,     // SC allocates 16 bits
    maintenance_margin: UD64, // SC allocates 16 bits

    last_price: UD64, // SC allocates 32 bits
    last_price_block: Option<u64>,
    last_price_timestamp: u64,

    mark_price: UD64, // SC allocates 32 bits
    mark_price_block: Option<u64>,
    mark_price_timestamp: u64,

    oracle_price: UD64, // SC allocates 32 bits
    oracle_price_block: Option<u64>,
    oracle_price_timestamp: u64,

    funding_start_block: u64,
    price_max_age: u64,

    orders: HashMap<types::OrderId, Order>,
    l2_book: L2Book,

    open_interest: UD64, // SC allocates 40 bits
}

impl Perpetual {
    pub(crate) fn new(
        instant: types::StateInstant,
        id: types::PerpetualId,
        info: &PerpetualInfo,
        maker_fee: U256,
        taker_fee: U256,
        initial_margin: U256,
        maintenance_margin: U256,
    ) -> Self {
        let price_converter = num::Converter::new(info.priceDecimals.to());
        let size_converter = num::Converter::new(info.lotDecimals.to());
        let fee_converter = num::Converter::new(FEE_SCALE);
        let leverage_converter = num::Converter::new(LEVERAGE_SCALE);
        Self {
            instant,
            id,
            name: info.name.clone(),
            symbol: info.symbol.clone(),
            is_paused: info.paused,

            price_converter,
            size_converter,
            leverage_converter,
            base_price: price_converter.from_unsigned(info.basePricePNS),

            maker_fee: fee_converter.from_unsigned(maker_fee), // Fees are per 100K
            taker_fee: fee_converter.from_unsigned(taker_fee), // Fees are per 100K
            // Margins are in hundredths
            initial_margin: leverage_converter.from_unsigned(initial_margin),
            // Margins are in hundredths
            maintenance_margin: leverage_converter.from_unsigned(maintenance_margin),

            // In the current revision of SC "mark" means "last"
            last_price: price_converter.from_unsigned(info.markPNS),
            last_price_block: None,
            last_price_timestamp: info.markTimestamp.to(),

            // In this revision of SC "index" is used as mark price
            mark_price: price_converter.from_unsigned(info.indexPNS),
            mark_price_block: None,
            mark_price_timestamp: info.indexTimestamp.to(),

            oracle_price: price_converter.from_unsigned(info.oraclePNS),
            oracle_price_block: None,
            oracle_price_timestamp: info.oracleTimestampSec.to(),

            funding_start_block: info.fundingStartBlock.to(),
            price_max_age: info.refPriceMaxAgeSec.to(),

            orders: HashMap::new(),
            l2_book: L2Book::new(),

            open_interest: size_converter.from_unsigned(info.longOpenInterestLNS),
        }
    }

    /// Instant the perpetual contract state is consistent with or was last updated at.
    pub fn instant(&self) -> types::StateInstant {
        self.instant
    }

    /// ID of the perpetual contract.
    pub fn id(&self) -> types::PerpetualId {
        self.id
    }

    /// Name of the perpetual contract.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Symbol of the perpetual contract.
    pub fn symbol(&self) -> String {
        self.symbol.clone()
    }

    /// Indicates if the perpetual contract is paused.
    pub fn is_paused(&self) -> bool {
        self.is_paused
    }

    /// Converter of prices between internal fixed-point and decimal representations.
    pub fn price_converter(&self) -> num::Converter {
        self.price_converter
    }

    /// Converter of sizes between internal fixed-point and decimal representations.
    pub fn size_converter(&self) -> num::Converter {
        self.size_converter
    }

    /// Converter of leverage/margin between internal fixed-point and decimal representations.
    pub fn leverage_converter(&self) -> num::Converter {
        self.leverage_converter
    }

    /// Maker fee, gets collected only on position opening/increasing.
    pub fn maker_fee(&self) -> UD64 {
        self.maker_fee
    }

    /// Taker fee, gets collected only on position opening/increasing.
    pub fn taker_fee(&self) -> UD64 {
        self.taker_fee
    }

    /// Minimal initial margin fraction required to open a position.
    pub fn initial_margin(&self) -> UD64 {
        self.initial_margin
    }

    /// Minimal maintenance margin fraction required to keep a position.
    pub fn maintenance_margin(&self) -> UD64 {
        self.maintenance_margin
    }

    /// The price last trade was executed at.
    pub fn last_price(&self) -> UD64 {
        self.last_price
    }

    /// The block number of the last trade.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn last_price_block(&self) -> Option<u64> {
        self.last_price_block
    }

    /// Unix timestamp (in seconds) of the last trade.
    pub fn last_price_timestamp(&self) -> u64 {
        self.last_price_timestamp
    }

    /// Mark price of the contract.
    pub fn mark_price(&self) -> UD64 {
        self.mark_price
    }

    /// The block number of the most recent mark price update.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn mark_price_block(&self) -> Option<u64> {
        self.mark_price_block
    }

    /// Unix timestamp (in seconds) of the most recent mark price update.
    pub fn mark_price_timestamp(&self) -> u64 {
        self.mark_price_timestamp
    }

    /// Indicates that the mark price is obsolete and will not be accepted
    /// during the order/position settlement
    pub fn is_mark_price_obsolete(&self) -> bool {
        self.mark_price_timestamp + self.price_max_age <= self.instant.block_timestamp()
    }

    /// Oracle price of the contract.
    pub fn oracle_price(&self) -> UD64 {
        self.oracle_price
    }

    /// The block number of the most recent oracle price update.
    /// Available only from real-time events, not from the initial snapshot.
    pub fn oracle_price_block(&self) -> Option<u64> {
        self.oracle_price_block
    }

    /// Unix timestamp (in seconds) of the most recent oracle price update.
    pub fn oracle_price_timestamp(&self) -> u64 {
        self.oracle_price_timestamp
    }

    /// Indicates that the oracle price is obsolete and will not be accepted
    /// during the order/position settlement
    pub fn is_oracle_price_obsolete(&self) -> bool {
        self.oracle_price_timestamp + self.price_max_age <= self.instant.block_timestamp()
    }

    /// Starting block number of funding intervals.
    /// Use [`Exchange::funding_interval_blocks`] to get interval "duration" in blocks.
    pub fn funding_start_block(&self) -> u64 {
        self.funding_start_block
    }

    /// Active orders in the perpetual contract book.
    pub fn orders(&self) -> &HashMap<types::OrderId, Order> {
        &self.orders
    }

    /// Up to date L2 order book.
    pub fn l2_book(&self) -> &L2Book {
        &self.l2_book
    }

    /// Open interest in the perpetual contract.
    pub fn open_interest(&self) -> UD64 {
        self.open_interest
    }

    pub(crate) fn base_price(&self) -> UD64 {
        self.base_price
    }

    pub(crate) fn add_order(&mut self, order: Order) {
        self.l2_book.add_order(&order);
        self.orders.insert(order.order_id(), order);
    }
}
