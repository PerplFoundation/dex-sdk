use alloy::primitives::U256;
use fastnum::{UD64, UD128};

use crate::{abi::dex::Exchange::OrderDesc, num, state};

use super::*;

/// Type of the order request.
///
/// * [`RequestType::OpenLong`] is used to open a long position (or to decrease, close, or invert a long
///   position). The only restrictions applied are the user account must have sufficient
///   collateral available.
/// * [`RequestType::OpenShort`] is used to open a short position (or to decrease, close, or invert a
///   short position). The only restrictions applied are the user account must have
///   sufficient collateral available.
/// * [`RequestType::CloseLong`] is a reduce only order type and can only be used to close all or part of
///   an existing long position on the perpetual contract.
/// * [`RequestType::CloseShort`] is a reduce only order type and can only be used to close all or part of
///   an existing short position on the perpetual contract.
/// * [`RequestType::Cancel`] is used to cancel an existing order on the perpetual contract's order book.
/// * [`RequestType::IncreasePositionCollateral`] is an operation to increase the collateral of an existing
///   position in the event that it has insufficient margin or the account holder wishes to
///   reduce leverage.
/// * [`RequestType::Change`] is an operation to change parameters of an existing order, gas-efficiently.
#[derive(Clone, Copy, Debug)]
pub enum RequestType {
    OpenLong,
    OpenShort,
    CloseLong,
    CloseShort,
    Cancel,
    IncreasePositionCollateral,
    Change,
}

/// Request to post/modify an order.
#[derive(Clone, derive_more::Debug)]
pub struct OrderRequest {
    request_id: RequestId,
    perp_id: PerpetualId,
    r#type: RequestType,
    order_id: Option<OrderId>,
    #[debug("{price}")]
    price: UD64,
    #[debug("{size}")]
    size: UD64,
    expiry_block: Option<u64>,
    post_only: bool,
    fill_or_kill: bool,
    immediate_or_cancel: bool,
    max_matches: Option<u32>,
    #[debug("{leverage}")]
    leverage: UD64,
    last_exec_block: Option<u64>,
    amount: Option<UD128>,
}

impl OrderRequest {
    /// Create a new order request with provided parameters.
    ///
    /// Use [`Self::prepare`] to get [`OrderDesc`]s and then issue transactions with
    /// [`crate::abi::dex::Exchange::ExchangeInstance::execOpsAndOrders`] calls.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: RequestId,
        perp_id: PerpetualId,
        r#type: RequestType,
        order_id: Option<OrderId>,
        price: UD64,
        size: UD64,
        expiry_block: Option<u64>,
        post_only: bool,
        fill_or_kill: bool,
        immediate_or_cancel: bool,
        max_matches: Option<u32>,
        leverage: UD64,
        last_exec_block: Option<u64>,
        amount: Option<UD128>,
    ) -> Self {
        Self {
            request_id,
            perp_id,
            r#type,
            order_id,
            price,
            size,
            expiry_block,
            post_only,
            fill_or_kill,
            immediate_or_cancel,
            max_matches,
            leverage,
            last_exec_block,
            amount,
        }
    }

    /// Prepare order request to execution.
    pub fn prepare(&self, exchange: &state::Exchange) -> OrderDesc {
        let perp = exchange
            .perpetuals()
            .get(&self.perp_id)
            .expect("known perpetual");
        self.to_order_desc(
            perp.price_converter(),
            perp.size_converter(),
            perp.leverage_converter(),
            Some(exchange.collateral_converter()),
        )
    }

    pub(crate) fn to_order_desc(
        &self,
        price_converter: num::Converter,
        size_converter: num::Converter,
        leverage_converter: num::Converter,
        collateral_converter: Option<num::Converter>,
    ) -> OrderDesc {
        OrderDesc {
            orderDescId: U256::from(self.request_id),
            perpId: U256::from(self.perp_id),
            orderType: self.r#type as u8,
            orderId: U256::from(self.order_id.unwrap_or_default()),
            pricePNS: price_converter.to_unsigned(self.price),
            lotLNS: size_converter.to_unsigned(self.size),
            expiryBlock: U256::from(self.expiry_block.unwrap_or_default()),
            postOnly: self.post_only,
            fillOrKill: self.fill_or_kill,
            immediateOrCancel: self.immediate_or_cancel,
            maxMatches: U256::from(self.max_matches.unwrap_or_default()),
            leverageHdths: leverage_converter.to_unsigned(self.leverage),
            lastExecutionBlock: U256::from(self.last_exec_block.unwrap_or_default()),
            amountCNS: self
                .amount
                .zip(collateral_converter)
                .map(|(a, conv)| conv.to_unsigned(a))
                .unwrap_or_default(),
        }
    }
}

impl From<u8> for RequestType {
    fn from(value: u8) -> Self {
        match value {
            0 => RequestType::OpenLong,
            1 => RequestType::OpenShort,
            2 => RequestType::CloseLong,
            3 => RequestType::CloseShort,
            4 => RequestType::Cancel,
            5 => RequestType::IncreasePositionCollateral,
            6 => RequestType::Change,
            _ => unreachable!(),
        }
    }
}

impl From<RequestType> for OrderType {
    fn from(value: RequestType) -> Self {
        match value {
            RequestType::OpenLong => OrderType::OpenLong,
            RequestType::OpenShort => OrderType::OpenShort,
            RequestType::CloseLong => OrderType::CloseLong,
            RequestType::CloseShort => OrderType::CloseShort,
            _ => unreachable!(),
        }
    }
}
