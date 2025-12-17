//! Trade listener module for streaming normalized trade events.
//!
//! Listens to `MakerOrderFilled` and `TakerOrderFilled` events, batches all
//! maker fills per taker into unified `TakerTrade` events, normalizes
//! fixed-point values to decimals, and streams trades batched per block.
//!
//! # Architecture
//!
//! The module separates pure processing logic from async I/O:
//!
//! - [`TradeProcessor`] - Pure, synchronous trade extraction from raw events
//! - [`NormalizationConfig`] - Configuration fetched once at startup
//! - [`start`] - Async entry point that spawns a background listener task
//!
//! # Data Model
//!
//! Each [`TakerTrade`] represents a single taker order execution that may have
//! matched against multiple maker orders. The `maker_fills` vector contains
//! all individual [`MakerFill`]s that occurred as part of this trade.
//!
//! # Example
//!
//! ```ignore
//! use dex_sdk::{Chain, fill, types::StateInstant};
//!
//! let chain = Chain::testnet();
//! let provider = /* setup provider */;
//! let from = StateInstant::new(latest_block, timestamp);
//!
//! let (mut rx, handle) = fill::start(&chain, provider, from, tokio::time::sleep).await?;
//!
//! while let Some(block_trades) = rx.recv().await {
//!     println!("Block {}: {} trades",
//!         block_trades.instant.block_number(),
//!         block_trades.trades.len()
//!     );
//!
//!     for trade in &block_trades.trades {
//!         println!("Taker {} {:?} on perp {} (fee: {})",
//!             trade.taker_account_id, trade.taker_side,
//!             trade.perpetual_id, trade.taker_fee);
//!         for fill in &trade.maker_fills {
//!             println!("  Maker {} order {} filled {} @ {} (fee: {})",
//!                 fill.maker_account_id, fill.maker_order_id,
//!                 fill.size, fill.price, fill.fee);
//!         }
//!     }
//! }
//!
//! // Check for errors
//! handle.await??;
//! ```

mod listener;
mod types;

pub use listener::{NormalizationConfig, TradeProcessor, start};
pub use types::{BlockTrades, MakerFill, TakerTrade, TradeReceiver};
