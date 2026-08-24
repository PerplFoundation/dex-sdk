use fastnum::UD64;

use super::BuilderAttribution;

/// A single maker fill within a taker trade.
#[derive(Clone, derive_more::Debug)]
pub struct MakerFill {
    /// Log index of this maker fill event.
    pub log_index: u64,

    /// Maker account ID.
    pub maker_account_id: super::AccountId,

    /// Maker order ID.
    pub maker_order_id: super::OrderId,

    /// Maker client order ID, if known.
    ///
    /// Available only when the order placement was observed in processed
    /// events, not for orders loaded from the initial snapshot.
    pub maker_client_order_id: Option<super::RequestId>,

    /// Fill price (normalized decimal).
    #[debug("{price}")]
    pub price: UD64,

    /// Fill size (normalized decimal).
    #[debug("{size}")]
    pub size: UD64,

    /// Maker fee paid (normalized decimal, in collateral token).
    #[debug("{fee}")]
    pub fee: UD64,

    /// Builder the maker order is attributed to, with the fee rate it charges,
    /// if any.
    ///
    /// Available only when the order placement was observed in processed events
    /// or recovered from the initial snapshot, on contract v1.1.7.4+.
    pub builder: Option<super::BuilderAttribution>,

    /// Builder fee earned on this fill (normalized decimal, in collateral
    /// token).
    ///
    /// Included in [`Self::fee`], so consumers must not add it on top. Zero on
    /// close/decrease fills and on contracts without builder attribution, even
    /// when [`Self::builder`] is set.
    #[debug("{builder_fee}")]
    pub builder_fee: UD64,
}

/// A complete trade event: one taker matched against one or more makers.
///
/// Each `TakerTrade` represents a single taker order execution that may have
/// matched against multiple maker orders. The `maker_fills` vector contains
/// all individual maker fills that occurred as part of this trade.
#[derive(Clone, derive_more::Debug)]
pub struct Trade {
    /// Perpetual contract ID.
    pub perpetual_id: super::PerpetualId,

    /// Taker account ID.
    pub taker_account_id: super::AccountId,

    /// Taker request ID.
    pub taker_request_id: super::RequestId,

    /// Taker side (Bid = buying, Ask = selling).
    pub taker_side: super::OrderSide,

    /// Taker fee paid (normalized decimal, in collateral token).
    #[debug("{taker_fee}")]
    pub taker_fee: UD64,

    /// Builder the taker order is attributed to, with the fee rate it charges,
    /// if any.
    pub taker_builder: Option<BuilderAttribution>,

    /// Builder fee earned on the taker side (normalized decimal, in collateral
    /// token).
    ///
    /// Included in [`Self::taker_fee`], so consumers must not add it on top.
    #[debug("{taker_builder_fee}")]
    pub taker_builder_fee: UD64,

    /// All maker fills matched by this taker order.
    pub maker_fills: Vec<MakerFill>,
}

impl Trade {
    /// Total size filled across all makers.
    pub fn total_size(&self) -> UD64 {
        self.maker_fills.iter().map(|f| f.size).sum()
    }

    /// Volume-weighted average price across all maker fills.
    ///
    /// Returns `None` if there are no fills.
    pub fn avg_price(&self) -> Option<UD64> {
        if self.maker_fills.is_empty() {
            return None;
        }
        let total_value: UD64 = self.maker_fills.iter().map(|f| f.price * f.size).sum();
        let total_size = self.total_size();
        if total_size == UD64::ZERO {
            return None;
        }
        Some(total_value / total_size)
    }

    /// Total maker fees paid across all fills.
    pub fn total_maker_fees(&self) -> UD64 {
        self.maker_fills.iter().map(|f| f.fee).sum()
    }

    /// Total builder fees earned on this trade, taker and maker sides combined.
    ///
    /// Part of [`Self::taker_fee`] and the maker fees, not additional to them.
    pub fn total_builder_fees(&self) -> UD64 {
        self.taker_builder_fee + self.maker_fills.iter().map(|f| f.builder_fee).sum::<UD64>()
    }

    /// Total builder fees earned by a specific builder on this trade.
    pub fn builder_total(&self, builder_id: super::BuilderId) -> UD64 {
        let taker = self
            .taker_builder
            .filter(|b| b.builder_id() == builder_id)
            .map(|_| self.taker_builder_fee)
            .unwrap_or(UD64::ZERO);
        taker
            + self
                .maker_fills
                .iter()
                .filter(|f| f.builder.is_some_and(|b| b.builder_id() == builder_id))
                .map(|f| f.builder_fee)
                .sum::<UD64>()
    }

    /// Volume-weighted average price, total size and total fees for a specific
    /// maker.
    ///
    /// Returns `None` if the maker has no fills in this trade.
    pub fn maker_total(&self, account_id: super::AccountId) -> Option<(UD64, UD64, UD64)> {
        let mut total_value = UD64::ZERO;
        let mut total_size = UD64::ZERO;
        let mut total_fee = UD64::ZERO;
        for fill in self
            .maker_fills
            .iter()
            .filter(|f| f.maker_account_id == account_id)
        {
            total_value += fill.price * fill.size;
            total_size += fill.size;
            total_fee += fill.fee
        }
        if total_size == UD64::ZERO {
            return None;
        }
        Some((total_value / total_size, total_size, total_fee))
    }
}
