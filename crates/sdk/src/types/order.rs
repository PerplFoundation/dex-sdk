/// Type of the placed order.
///
/// Bid Order Types:
/// * [`OrderType::OpenLong`] is used to open a long position (or to decrease, close, or invert a long
///   position). The only restrictions applied are the user account must have sufficient
///   collateral available.
/// * [`OrderType::CloseShort`] is a reduce only order type and can only be used to close all or part of
///   an existing short position on the perpetual contract.
///
/// Ask Order Types:
/// * [`OrderType::OpenShort`] is used to open a short position (or to decrease, close, or invert a
///   short position). The only restrictions applied are the user account must have
///   sufficient collateral available.
/// * [`OrderType::CloseLong`] is a reduce only order type and can only be used to close all or part of
///   an existing long position on the perpetual contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum OrderType {
    OpenLong,
    OpenShort,
    CloseLong,
    CloseShort,
}

/// Side of the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderSide {
    Ask,
    Bid,
}

impl OrderType {
    pub fn side(&self) -> OrderSide {
        match self {
            OrderType::OpenLong | OrderType::CloseShort => OrderSide::Bid,
            OrderType::OpenShort | OrderType::CloseLong => OrderSide::Ask,
        }
    }
}

impl From<u8> for OrderType {
    fn from(value: u8) -> Self {
        match value {
            0 => OrderType::OpenLong,
            1 => OrderType::OpenShort,
            2 => OrderType::CloseLong,
            3 => OrderType::CloseShort,
            _ => unreachable!(),
        }
    }
}
