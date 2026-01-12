use std::fmt::Display;

/// Type of the placed order.
///
/// Bid Order Types:
/// * [`OrderType::OpenLong`] is used to open a long position (or to decrease,
///   close, or invert a long position). The only restrictions applied are the
///   user account must have sufficient collateral available.
/// * [`OrderType::CloseShort`] is a reduce only order type and can only be used
///   to close all or part of an existing short position on the perpetual
///   contract.
///
/// Ask Order Types:
/// * [`OrderType::OpenShort`] is used to open a short position (or to decrease,
///   close, or invert a short position). The only restrictions applied are the
///   user account must have sufficient collateral available.
/// * [`OrderType::CloseLong`] is a reduce only order type and can only be used
///   to close all or part of an existing long position on the perpetual
///   contract.
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

impl OrderSide {
    pub fn opposite(&self) -> OrderSide {
        match self {
            OrderSide::Ask => OrderSide::Bid,
            OrderSide::Bid => OrderSide::Ask,
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

impl Display for OrderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            match self {
                OrderType::OpenLong => write!(f, "OL"),
                OrderType::OpenShort => write!(f, "OS"),
                OrderType::CloseLong => write!(f, "CL"),
                OrderType::CloseShort => write!(f, "CS"),
            }
        } else {
            match self {
                OrderType::OpenLong => write!(f, "Open Long"),
                OrderType::OpenShort => write!(f, "Open Short"),
                OrderType::CloseLong => write!(f, "Close Long"),
                OrderType::CloseShort => write!(f, "Close Short"),
            }
        }
    }
}

impl Display for OrderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderSide::Ask => write!(f, "Ask"),
            OrderSide::Bid => write!(f, "Bid"),
        }
    }
}
