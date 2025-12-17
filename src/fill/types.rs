//! Fill data structures.

use alloy::primitives::TxHash;
use fastnum::UD64;
use tokio::sync::mpsc;

use crate::types::{self, OrderSide};

/// A single maker fill within a taker trade.
#[derive(Clone, Debug)]
pub struct MakerFill {
    /// Log index of this maker fill event.
    pub log_index: u64,

    /// Maker account ID.
    pub maker_account_id: types::AccountId,

    /// Maker order ID.
    pub maker_order_id: types::OrderId,

    /// Fill price (normalized decimal).
    pub price: UD64,

    /// Fill size (normalized decimal).
    pub size: UD64,

    /// Maker fee paid (normalized decimal, in collateral token).
    pub fee: UD64,
}

/// A complete trade event: one taker matched against one or more makers.
///
/// Each `TakerTrade` represents a single taker order execution that may have
/// matched against multiple maker orders. The `maker_fills` vector contains
/// all individual maker fills that occurred as part of this trade.
#[derive(Clone, Debug)]
pub struct TakerTrade {
    /// Transaction hash the trade occurred in.
    pub tx_hash: TxHash,

    /// Transaction index within the block.
    pub tx_index: u64,

    /// Perpetual contract ID.
    pub perpetual_id: types::PerpetualId,

    /// Taker account ID.
    pub taker_account_id: types::AccountId,

    /// Taker side (Bid = buying, Ask = selling).
    pub taker_side: OrderSide,

    /// Taker fee paid (normalized decimal, in collateral token).
    pub taker_fee: UD64,

    /// All maker fills matched by this taker order.
    pub maker_fills: Vec<MakerFill>,
}

impl TakerTrade {
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

/// Trades from a single block.
#[derive(Clone, Debug)]
pub struct BlockTrades {
    /// Block instant.
    pub instant: types::StateInstant,

    /// All trades in this block.
    pub trades: Vec<TakerTrade>,
}

impl BlockTrades {
    pub(crate) fn new(instant: types::StateInstant, trades: Vec<TakerTrade>) -> Self {
        Self { instant, trades }
    }

    /// Returns true if there are no trades in this block.
    pub fn is_empty(&self) -> bool {
        self.trades.is_empty()
    }

    /// Returns the number of trades in this block.
    pub fn len(&self) -> usize {
        self.trades.len()
    }
}

/// Receiver for block trades.
pub struct TradeReceiver {
    inner: mpsc::Receiver<BlockTrades>,
}

impl TradeReceiver {
    pub(crate) fn new(inner: mpsc::Receiver<BlockTrades>) -> Self {
        Self { inner }
    }

    /// Receives the next batch of trades, or `None` if the channel is closed.
    pub async fn recv(&mut self) -> Option<BlockTrades> {
        self.inner.recv().await
    }
}
