use std::collections::hash_map::Entry;

use super::*;
use crate::{abi::dex::Exchange::PerpetualInfo, types};
use alloy::primitives::U256;
use fastnum::{UD64, UD128};

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
    fee_converter: num::Converter,
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

    open_interest: UD128,
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
            fee_converter,
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

    /// Converter of fees between internal fixed-point and decimal representations.
    pub fn fee_converter(&self) -> num::Converter {
        self.fee_converter
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
    pub fn open_interest(&self) -> UD128 {
        self.open_interest
    }

    pub(crate) fn base_price(&self) -> UD64 {
        self.base_price
    }

    pub(crate) fn add_order(&mut self, order: Order) {
        self.l2_book.add_order(&order);
        self.orders.insert(order.order_id(), order);
    }

    pub(crate) fn update_order(&mut self, order: Order) -> Result<(), DexError> {
        match self.orders.entry(order.order_id()) {
            Entry::Occupied(mut e) => {
                let prev = e.get();
                if prev.price() != order.price() {
                    self.l2_book.remove_order(prev);
                    self.l2_book.add_order(&order);
                } else {
                    self.l2_book.update_order(&order, prev.size());
                }
                e.insert(order);
                Ok(())
            }
            Entry::Vacant(_) => Err(DexError::OrderNotFound(self.id, order.order_id())),
        }
    }

    pub(crate) fn remove_order(&mut self, order_id: types::OrderId) -> Result<Order, DexError> {
        match self.orders.entry(order_id) {
            Entry::Occupied(e) => {
                self.l2_book.remove_order(e.get());
                Ok(e.remove())
            }
            Entry::Vacant(_) => Err(DexError::OrderNotFound(self.id, order_id)),
        }
    }

    pub(crate) fn update_paused(&mut self, instant: types::StateInstant, paused: bool) {
        self.is_paused = paused;
        self.instant = instant;
    }

    pub(crate) fn update_maker_fee(&mut self, instant: types::StateInstant, maker_fee: UD64) {
        self.maker_fee = maker_fee;
        self.instant = instant;
    }

    pub(crate) fn update_taker_fee(&mut self, instant: types::StateInstant, taker_fee: UD64) {
        self.taker_fee = taker_fee;
        self.instant = instant;
    }

    pub(crate) fn update_initial_margin(
        &mut self,
        instant: types::StateInstant,
        initial_margin: UD64,
    ) {
        self.initial_margin = initial_margin;
        self.instant = instant;
    }

    pub(crate) fn update_maintenance_margin(
        &mut self,
        instant: types::StateInstant,
        maintenance_margin: UD64,
    ) {
        self.maintenance_margin = maintenance_margin;
        self.instant = instant;
    }

    pub(crate) fn update_last_price(&mut self, instant: types::StateInstant, last_price: UD64) {
        self.last_price = last_price;
        self.last_price_block = Some(instant.block_number());
        self.last_price_timestamp = instant.block_timestamp();
        self.instant = instant;
    }

    pub(crate) fn update_mark_price(&mut self, instant: types::StateInstant, mark_price: UD64) {
        self.mark_price = mark_price;
        self.mark_price_block = Some(instant.block_number());
        self.mark_price_timestamp = instant.block_timestamp();
        self.instant = instant;
    }

    pub(crate) fn update_oracle_price(&mut self, instant: types::StateInstant, oracle_price: UD64) {
        self.oracle_price = oracle_price;
        self.oracle_price_block = Some(instant.block_number());
        self.oracle_price_timestamp = instant.block_timestamp();
        self.instant = instant;
    }

    pub(crate) fn update_price_max_age(
        &mut self,
        instant: types::StateInstant,
        price_max_age: u64,
    ) {
        self.price_max_age = price_max_age;
        self.instant = instant;
    }

    pub(crate) fn update_open_interest(
        &mut self,
        instant: types::StateInstant,
        prev_size: UD64,
        new_size: UD64,
    ) {
        self.open_interest -= prev_size.resize();
        self.open_interest += new_size.resize();
        self.instant = instant;
    }
}
