use super::*;
use crate::{abi::dex::Exchange::PerpetualInfo, types};
use alloy::primitives::{B256, I256, U256};
use fastnum::{D64, D256, UD64, UD128};

const FEE_SCALE: u8 = 5;
const FUNDING_RATE_SCALE: u8 = 5;
const LEVERAGE_SCALE: u8 = 2;

/// Perpetual contract tradeable at the exchange.
///
/// Provides the current state of contract parameters, market data and
/// order book.
#[derive(Clone, derive_more::Debug)]
pub struct Perpetual {
    instant: types::StateInstant,
    state_instant: types::StateInstant,
    id: types::PerpetualId,
    name: String,
    symbol: String,
    is_paused: bool,

    price_converter: num::Converter,
    size_converter: num::Converter,
    leverage_converter: num::Converter,
    fee_converter: num::Converter,
    funding_rate_converter: num::Converter,
    #[debug("{base_price}")]
    base_price: UD64, // SC allocates 32 bits

    #[debug("{maker_fee}")]
    maker_fee: UD64, // SC allocates 16 bits
    #[debug("{taker_fee}")]
    taker_fee: UD64, // SC allocates 16 bits
    #[debug("{initial_margin}")]
    initial_margin: UD64, // SC allocates 16 bits
    #[debug("{maintenance_margin}")]
    maintenance_margin: UD64, // SC allocates 16 bits

    #[debug("{last_price}")]
    last_price: UD64, // SC allocates 32 bits
    last_price_block: Option<u64>,
    last_price_timestamp: u64,

    #[debug("{mark_price}")]
    mark_price: UD64, // SC allocates 32 bits
    mark_price_block: Option<u64>,
    mark_price_timestamp: u64,

    #[debug("{oracle_price}")]
    oracle_price: UD64, // SC allocates 32 bits
    oracle_price_block: Option<u64>,
    oracle_price_timestamp: u64,

    #[debug("{prev_funding_rate}")]
    prev_funding_rate: D64, // SC allocates 16 bits of precision
    #[debug("{:?}", next_funding_rate.map(|v| format!("{v}")))]
    next_funding_rate: Option<D64>, // SC allocates 16 bits of precision
    #[debug("{:?}", next_funding_payment.map(|v| format!("{v}")))]
    next_funding_payment: Option<D256>, // SC allocates 48 bits of precision
    next_funding_event_block: Option<u64>,
    funding_start_block: u64,

    oracle_feed_id: B256,
    is_oracle_used: bool,
    price_max_age_sec: u64,

    l2_book: OrderBook,

    #[debug("{open_interest}")]
    open_interest: UD128,
}

impl Perpetual {
    #[allow(clippy::too_many_arguments)]
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
        let leverage_converter = num::Converter::new(LEVERAGE_SCALE);
        let fee_converter = num::Converter::new(FEE_SCALE);
        let funding_rate_converter = num::Converter::new(FUNDING_RATE_SCALE);
        Self {
            instant,
            state_instant: instant,
            id,
            name: info.name.clone(),
            symbol: info.symbol.clone(),
            is_paused: info.paused,

            price_converter,
            size_converter,
            leverage_converter,
            fee_converter,
            funding_rate_converter,
            base_price: price_converter.from_unsigned(info.basePricePNS),

            maker_fee: fee_converter.from_unsigned(maker_fee), // Fees are per 100K
            taker_fee: fee_converter.from_unsigned(taker_fee), // Fees are per 100K
            // Margins are in hundredths
            initial_margin: leverage_converter.from_unsigned(initial_margin),
            // Margins are in hundredths
            maintenance_margin: leverage_converter.from_unsigned(maintenance_margin),

            last_price: price_converter.from_unsigned(info.lastPNS),
            last_price_block: None,
            last_price_timestamp: info.lastTimestamp.to(),

            mark_price: price_converter.from_unsigned(info.markPNS),
            mark_price_block: None,
            mark_price_timestamp: info.markTimestamp.to(),

            oracle_price: price_converter.from_unsigned(info.oraclePNS),
            oracle_price_block: None,
            oracle_price_timestamp: info.oracleTimestampSec.to(),

            prev_funding_rate: funding_rate_converter
                .from_signed(I256::try_from(info.fundingRatePct100k).unwrap()),
            next_funding_rate: None,
            next_funding_payment: None,
            next_funding_event_block: None,
            funding_start_block: info.fundingStartBlock.to(),

            oracle_feed_id: info.linkFeedId,
            is_oracle_used: !info.ignOracle,
            price_max_age_sec: info.refPriceMaxAgeSec.to(),

            l2_book: OrderBook::new(),

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

    /// Converter of funding rates between internal fixed-point and decimal representations.
    pub fn funding_rate_converter(&self) -> num::Converter {
        self.funding_rate_converter
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
        self.mark_price_timestamp + self.price_max_age_sec <= self.instant.block_timestamp()
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
        self.oracle_price_timestamp + self.price_max_age_sec <= self.instant.block_timestamp()
    }

    /// The funding rate applied at the previous funding event.
    pub fn funding_rate(&self) -> D64 {
        if let Some((next, bl)) = self.next_funding_rate.zip(self.next_funding_event_block)
            && bl <= self.state_instant.block_number()
        {
            next
        } else {
            self.prev_funding_rate
        }
    }

    /// If the next funding rate has been set.
    pub fn has_next_funding_rate(&self) -> bool {
        self.next_funding_rate.is_some()
            && self
                .next_funding_event_block
                .is_some_and(|bl| bl > self.state_instant.block_number())
    }

    /// Starting block number of funding intervals.
    /// Use [`Exchange::funding_interval_blocks`] to get interval "duration" in blocks.
    pub fn funding_start_block(&self) -> u64 {
        self.funding_start_block
    }

    /// Feed ID of ChainLink DataStreams price oracle.
    pub fn oracle_feed_id(&self) -> B256 {
        self.oracle_feed_id
    }

    /// If perpetual contract relues on oracle prices.
    pub fn is_oracle_used(&self) -> bool {
        self.is_oracle_used
    }

    /// Max age in seconds for oracle/mark prices.
    pub fn price_max_age_sec(&self) -> u64 {
        self.price_max_age_sec
    }

    /// Get a specific order by ID.
    pub fn get_order(&self, order_id: types::OrderId) -> Option<&Order> {
        self.l2_book.get_order_data(order_id)
    }

    /// Total number of orders in the book.
    pub fn total_orders(&self) -> usize {
        self.l2_book.total_orders()
    }

    /// Up to date L2 order book.
    pub fn l2_book(&self) -> &OrderBook {
        &self.l2_book
    }

    /// Open interest in the perpetual contract.
    pub fn open_interest(&self) -> UD128 {
        self.open_interest
    }

    pub(crate) fn base_price(&self) -> UD64 {
        self.base_price
    }

    pub(crate) fn update_state_instant(
        &mut self,
        instant: types::StateInstant,
    ) -> Vec<StateEvents> {
        self.state_instant = instant;
        if let Some(payment) = self.next_funding_payment
            && self
                .next_funding_event_block
                .is_some_and(|fe| fe == instant.block_number())
        {
            vec![StateEvents::perpetual(
                self,
                PerpetualEventType::FundingEvent {
                    rate: self.funding_rate(),
                    payment_per_unit: payment,
                },
            )]
        } else {
            vec![]
        }
    }

    pub(crate) fn add_order(&mut self, order: Order) -> Result<(), DexError> {
        self.l2_book.add_order(&order)?;
        Ok(())
    }

    /// Add orders from a snapshot, reconstructing FIFO order from linked list pointers.
    ///
    /// Uses the `prev_order_id`/`next_order_id` fields from the snapshot to determine
    /// the correct queue position within each price level.
    pub(crate) fn add_orders_from_snapshot(&mut self, orders: Vec<Order>) -> Result<(), DexError> {
        self.l2_book.add_orders_from_snapshot(&orders)?;
        Ok(())
    }

    pub(crate) fn update_order(&mut self, order: Order) -> Result<(), DexError> {
        let prev = self
            .l2_book
            .get_order_data(order.order_id())
            .copied()
            .ok_or(DexError::OrderNotFound(self.id, order.order_id()))?;

        if prev.price() != order.price() {
            // Price changed: remove from old level, add to new level (back of queue)
            self.l2_book.remove_order_by_id(order.order_id())?;
            self.l2_book.add_order(&order)?;
        } else if order.size() > prev.size() {
            // Size INCREASED at same price: move to back of queue (loses priority)
            self.l2_book.move_to_back(&order, &prev)?;
        } else if prev.expiry_block() > 0
            && prev.expiry_block() < order.instant().block_number()
            && prev.expiry_block() != order.expiry_block()
        {
            // Expired order got new expiry: move to back of queue (loses priority)
            self.l2_book.move_to_back(&order, &prev)?;
        } else {
            // Size decreased or unchanged: keep queue position
            self.l2_book.update_order(&order, &prev)?;
        }
        Ok(())
    }

    pub(crate) fn remove_order(&mut self, order_id: types::OrderId) -> Result<Order, DexError> {
        self.l2_book
            .remove_order_by_id(order_id)
            .map_err(|_| DexError::OrderNotFound(self.id, order_id))
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

    pub(crate) fn update_funding(
        &mut self,
        instant: types::StateInstant,
        funding_rate: D64,
        funding_payment: D256,
        block_num: u64,
    ) {
        if let Some(next) = self.next_funding_rate
            && self
                .next_funding_event_block
                .expect("next_funding_event_block set")
                < block_num
        {
            self.prev_funding_rate = next;
        }
        self.next_funding_rate = Some(funding_rate);
        self.next_funding_payment = Some(funding_payment);
        self.next_funding_event_block = Some(block_num);
        self.instant = instant;
    }

    pub(crate) fn update_oracle_feed_id(
        &mut self,
        instant: types::StateInstant,
        oracle_feed_id: B256,
    ) {
        self.oracle_feed_id = oracle_feed_id;
        self.instant = instant;
    }

    pub(crate) fn update_is_oracle_used(
        &mut self,
        instant: types::StateInstant,
        is_oracle_used: bool,
    ) {
        self.is_oracle_used = is_oracle_used;
        self.instant = instant;
    }

    pub(crate) fn update_price_max_age_sec(
        &mut self,
        instant: types::StateInstant,
        price_max_age_sec: u64,
    ) {
        self.price_max_age_sec = price_max_age_sec;
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

    /// Create a minimal Perpetual for testing purposes.
    #[cfg(test)]
    pub(crate) fn for_testing(id: types::PerpetualId) -> Self {
        Self {
            instant: types::StateInstant::new(0, 0),
            state_instant: types::StateInstant::new(0, 0),
            id,
            name: "TEST".to_string(),
            symbol: "TEST".to_string(),
            is_paused: false,
            price_converter: num::Converter::new(0),
            size_converter: num::Converter::new(0),
            leverage_converter: num::Converter::new(2),
            fee_converter: num::Converter::new(5),
            funding_rate_converter: num::Converter::new(5),
            base_price: UD64::ZERO,
            maker_fee: UD64::ZERO,
            taker_fee: UD64::ZERO,
            initial_margin: UD64::ZERO,
            maintenance_margin: UD64::ZERO,
            last_price: UD64::ZERO,
            last_price_block: None,
            last_price_timestamp: 0,
            mark_price: UD64::ZERO,
            mark_price_block: None,
            mark_price_timestamp: 0,
            oracle_price: UD64::ZERO,
            oracle_price_block: None,
            oracle_price_timestamp: 0,
            prev_funding_rate: D64::ZERO,
            next_funding_rate: None,
            next_funding_payment: None,
            next_funding_event_block: None,
            funding_start_block: 0,
            oracle_feed_id: B256::ZERO,
            is_oracle_used: false,
            price_max_age_sec: 0,
            l2_book: OrderBook::new(),
            open_interest: UD128::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastnum::udec64;
    use std::num::NonZeroU16;

    fn oid(n: u16) -> types::OrderId {
        NonZeroU16::new(n).expect("test order id must be non-zero")
    }

    #[test]
    fn update_order_expired_order_renewal_moves_to_back() {
        let mut perp = Perpetual::for_testing(1);

        // Create two orders at the same price
        // Order 1: expires at block 100
        // Order 2: no expiry
        let order1 = Order::for_l3_testing(
            types::OrderType::OpenShort, // Ask
            udec64!(100),                // price
            udec64!(1.0),                // size
            50,                          // block_number
            oid(1),                      // order_id
            101,                         // account_id
        )
        .with_expiry_block(100);

        let order2 = Order::for_l3_testing(
            types::OrderType::OpenShort,
            udec64!(100),
            udec64!(2.0),
            50,
            oid(2),
            102,
        );

        // Add orders: FIFO is [1, 2]
        perp.add_order(order1).unwrap();
        perp.add_order(order2).unwrap();

        // Verify initial FIFO order
        let orders: Vec<_> = perp.l2_book.ask_orders().map(|o| o.order_id()).collect();
        assert_eq!(orders, vec![oid(1), oid(2)], "Initial FIFO should be [1, 2]");

        // Now simulate time passing: we're at block 150 (order 1 is expired at block 100)
        // Update order 1 with a new expiry (block 200)
        let order1_renewed = Order::for_l3_testing(
            types::OrderType::OpenShort,
            udec64!(100),
            udec64!(1.0),
            150, // current block
            oid(1),
            101,
        )
        .with_expiry_block(200); // new expiry

        perp.update_order(order1_renewed).unwrap();

        // Order 1 should have moved to back: FIFO is [2, 1]
        let orders: Vec<_> = perp.l2_book.ask_orders().map(|o| o.order_id()).collect();
        assert_eq!(
            orders,
            vec![oid(2), oid(1)],
            "After expiry renewal, FIFO should be [2, 1]"
        );
    }

    #[test]
    fn update_order_non_expired_order_keeps_position() {
        let mut perp = Perpetual::for_testing(1);

        // Create two orders at the same price
        // Order 1: expires at block 100
        let order1 = Order::for_l3_testing(
            types::OrderType::OpenShort,
            udec64!(100),
            udec64!(1.0),
            50,
            oid(1),
            101,
        )
        .with_expiry_block(100);

        let order2 = Order::for_l3_testing(
            types::OrderType::OpenShort,
            udec64!(100),
            udec64!(2.0),
            50,
            oid(2),
            102,
        );

        perp.add_order(order1).unwrap();
        perp.add_order(order2).unwrap();

        // Update order 1 at block 80 (NOT expired yet) with new expiry
        let order1_updated = Order::for_l3_testing(
            types::OrderType::OpenShort,
            udec64!(100),
            udec64!(1.0),
            80, // current block < expiry_block(100)
            oid(1),
            101,
        )
        .with_expiry_block(200);

        perp.update_order(order1_updated).unwrap();

        // Order 1 should keep its position: FIFO is [1, 2]
        let orders: Vec<_> = perp.l2_book.ask_orders().map(|o| o.order_id()).collect();
        assert_eq!(orders, vec![oid(1), oid(2)], "Non-expired order should keep position");
    }
}
