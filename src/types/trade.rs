use fastnum::UD64;

/// A single maker fill within a taker trade.
#[derive(Clone, derive_more::Debug)]
pub struct MakerFill {
    /// Log index of this maker fill event.
    pub log_index: u64,

    /// Maker account ID.
    pub maker_account_id: super::AccountId,

    /// Maker order ID.
    pub maker_order_id: super::OrderId,

    /// Fill price (normalized decimal).
    #[debug("{price}")]
    pub price: UD64,

    /// Fill size (normalized decimal).
    #[debug("{size}")]
    pub size: UD64,

    /// Maker fee paid (normalized decimal, in collateral token).
    #[debug("{fee}")]
    pub fee: UD64,
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

    /// Taker side (Bid = buying, Ask = selling).
    pub taker_side: super::OrderSide,

    /// Taker fee paid (normalized decimal, in collateral token).
    #[debug("{taker_fee}")]
    pub taker_fee: UD64,

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
}
