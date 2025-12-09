//! Error types for L2/L3 order book operations.

use std::fmt;

use fastnum::UD64;

use super::order::OrderSlot;
use crate::types::{OrderId, OrderSide};

/// Error type for L2/L3 order book operations.
#[derive(Debug, Clone, PartialEq)]
pub enum L2BookError {
    /// Attempted to add an order that already exists in the book.
    OrderAlreadyExists {
        order_id: OrderId,
        existing_price: UD64,
        existing_slot: OrderSlot,
    },

    /// Attempted to update or remove an order that doesn't exist.
    OrderNotFound { order_id: OrderId },

    /// Order exists in index but not found at the expected price level.
    /// This indicates internal inconsistency.
    OrderNotAtExpectedLevel {
        order_id: OrderId,
        expected_price: UD64,
        side: OrderSide,
    },

    /// Attempted to update an order but the new order has a different ID.
    OrderIdMismatch {
        expected: OrderId,
        actual: OrderId,
    },

    /// Order has zero or negative size.
    InvalidOrderSize { order_id: OrderId, size: UD64 },

    /// Order has zero price.
    InvalidOrderPrice { order_id: OrderId, price: UD64 },
}

impl fmt::Display for L2BookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            L2BookError::OrderAlreadyExists {
                order_id,
                existing_price,
                existing_slot,
            } => write!(
                f,
                "order {} already exists at price {} (slot {:?})",
                order_id, existing_price, existing_slot
            ),
            L2BookError::OrderNotFound { order_id } => {
                write!(f, "order {} not found in book", order_id)
            }
            L2BookError::OrderNotAtExpectedLevel {
                order_id,
                expected_price,
                side,
            } => write!(
                f,
                "order {} not found at expected {} level price {}",
                order_id,
                match side {
                    OrderSide::Ask => "ask",
                    OrderSide::Bid => "bid",
                },
                expected_price
            ),
            L2BookError::OrderIdMismatch { expected, actual } => {
                write!(f, "order ID mismatch: expected {}, got {}", expected, actual)
            }
            L2BookError::InvalidOrderSize { order_id, size } => {
                write!(f, "order {} has invalid size: {}", order_id, size)
            }
            L2BookError::InvalidOrderPrice { order_id, price } => {
                write!(f, "order {} has invalid price: {}", order_id, price)
            }
        }
    }
}

impl std::error::Error for L2BookError {}

/// Result type for L2Book operations.
pub type L2BookResult<T> = Result<T, L2BookError>;
