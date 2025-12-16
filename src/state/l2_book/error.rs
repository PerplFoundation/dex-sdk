//! Error types for order book operations.

use fastnum::UD64;
use thiserror::Error;

use crate::types::{OrderId, OrderSide};

/// Error type for order book operations.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum OrderBookError {
    /// Attempted to add an order that already exists in the book.
    #[error("order {order_id} already exists at price {existing_price}")]
    OrderAlreadyExists {
        order_id: OrderId,
        existing_price: UD64,
    },

    /// Attempted to update or remove an order that doesn't exist.
    #[error("order {order_id} not found in book")]
    OrderNotFound { order_id: OrderId },

    /// Order exists in index but not found at the expected price level.
    /// This indicates internal inconsistency.
    #[error("order {order_id} not found at expected {side:?} level price {expected_price}")]
    OrderNotAtExpectedLevel {
        order_id: OrderId,
        expected_price: UD64,
        side: OrderSide,
    },

    /// Attempted to update an order but the new order has a different ID.
    #[error("order ID mismatch: expected {expected}, got {actual}")]
    OrderIdMismatch { expected: OrderId, actual: OrderId },

    /// Order has zero or negative size.
    #[error("order {order_id} has invalid size: {size}")]
    InvalidOrderSize { order_id: OrderId, size: UD64 },

    /// Order has zero price.
    #[error("order {order_id} has invalid price: {price}")]
    InvalidOrderPrice { order_id: OrderId, price: UD64 },

    /// Expected price level not found. This indicates internal inconsistency.
    #[error("level not found at price {price} ({side:?} side)")]
    LevelNotFound { price: UD64, side: OrderSide },

    /// Order references another order that doesn't exist in the snapshot.
    /// This indicates data inconsistency.
    #[error("order {order_id} has dangling {pointer} reference to non-existent order {referenced_id}")]
    DanglingOrderReference {
        order_id: OrderId,
        referenced_id: OrderId,
        pointer: &'static str,
    },
}

/// Result type for OrderBook operations.
pub type OrderBookResult<T> = Result<T, OrderBookError>;
