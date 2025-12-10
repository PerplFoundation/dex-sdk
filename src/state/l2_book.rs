use std::{
    cmp::Reverse,
    collections::{BTreeMap, btree_map},
};

use super::*;
use fastnum::{UD64, UD128};
use itertools::{FoldWhile, Itertools};

/// Price level of L2 order book.
#[derive(Clone, derive_more::Debug, Default)]
pub struct L2Level {
    #[debug("{size}")]
    size: UD64,
    num_orders: u32,
}

/// BTreeMap-based L2 order book.
///
/// Tracks the book state by order updates and provides minimal statistics computation.
#[derive(Clone, derive_more::Debug, Default)]
pub struct L2Book {
    #[debug("{:?}",  asks.iter().map(|(k, v)| format!("{k}: {v:?}")).collect::<Vec<_>>())]
    asks: BTreeMap<UD64, L2Level>,
    #[debug("{:?}", bids.iter().map(|(k, v)| format!("{}: {v:?}", k.0)).collect::<Vec<_>>())]
    bids: BTreeMap<Reverse<UD64>, L2Level>,
}

impl L2Level {
    pub fn size(&self) -> UD64 {
        self.size
    }

    pub fn num_orders(&self) -> u32 {
        self.num_orders
    }

    fn add_order(&mut self, size: UD64) {
        self.size += size;
        self.num_orders += 1;
    }

    fn update_order(&mut self, prev_size: UD64, new_size: UD64) {
        self.size -= prev_size;
        self.size += new_size;
    }

    fn remove_order(&mut self, size: UD64) {
        self.size -= size;
        self.num_orders -= 1;
    }

    fn is_empty(&self) -> bool {
        self.num_orders == 0
    }
}

impl L2Book {
    pub(crate) fn new() -> Self {
        Self {
            asks: BTreeMap::new(),
            bids: BTreeMap::new(),
        }
    }

    /// Asks sorted await from the spread.
    pub fn asks(&self) -> &BTreeMap<UD64, L2Level> {
        &self.asks
    }

    /// Bids sorted await from the spread.
    pub fn bids(&self) -> &BTreeMap<Reverse<UD64>, L2Level> {
        &self.bids
    }

    /// Best ask price/size.
    pub fn best_ask(&self) -> Option<(UD64, UD64)> {
        self.asks.first_key_value().map(|(k, v)| (*k, v.size))
    }

    /// Best bid price/size.
    pub fn best_bid(&self) -> Option<(UD64, UD64)> {
        self.bids.first_key_value().map(|(k, v)| (k.0, v.size))
    }

    /// Ask impact price for the requested size, along with the fillable size and size-averaged price.
    pub fn ask_impact(&self, want_size: UD64) -> Option<(UD64, UD64, UD64)> {
        Self::impact(self.asks.iter(), want_size)
    }

    /// Bid impact price for the requested size, along with the fillable size and size-averaged price.
    pub fn bid_impact(&self, want_size: UD64) -> Option<(UD64, UD64, UD64)> {
        Self::impact(self.bids.iter().map(|(k, v)| (&k.0, v)), want_size)
    }

    pub(crate) fn add_order(&mut self, order: &Order) {
        match order.r#type().side() {
            types::OrderSide::Ask => match self.asks.entry(order.price()) {
                btree_map::Entry::Vacant(v) => {
                    v.insert(L2Level {
                        size: order.size(),
                        num_orders: 1,
                    });
                }
                btree_map::Entry::Occupied(mut o) => {
                    o.get_mut().add_order(order.size());
                }
            },
            types::OrderSide::Bid => match self.bids.entry(Reverse(order.price())) {
                btree_map::Entry::Vacant(v) => {
                    v.insert(L2Level {
                        size: order.size(),
                        num_orders: 1,
                    });
                }
                btree_map::Entry::Occupied(mut o) => {
                    o.get_mut().add_order(order.size());
                }
            },
        }
    }

    pub(crate) fn update_order(&mut self, order: &Order, prev_size: UD64) {
        match order.r#type().side() {
            types::OrderSide::Ask => match self.asks.entry(order.price()) {
                btree_map::Entry::Vacant(_) => unreachable!("Updating vacant L2 book level"),
                btree_map::Entry::Occupied(mut o) => {
                    o.get_mut().update_order(prev_size, order.size());
                }
            },
            types::OrderSide::Bid => match self.bids.entry(Reverse(order.price())) {
                btree_map::Entry::Vacant(_) => unreachable!("Updating vacant L2 book level"),
                btree_map::Entry::Occupied(mut o) => {
                    o.get_mut().update_order(prev_size, order.size());
                }
            },
        }
    }

    pub(crate) fn remove_order(&mut self, order: &Order) {
        match order.r#type().side() {
            types::OrderSide::Ask => match self.asks.entry(order.price()) {
                btree_map::Entry::Vacant(_) => unreachable!("Updating vacant L2 book level"),
                btree_map::Entry::Occupied(mut o) => {
                    o.get_mut().remove_order(order.size());
                    if o.get().is_empty() {
                        o.remove();
                    }
                }
            },
            types::OrderSide::Bid => match self.bids.entry(Reverse(order.price())) {
                btree_map::Entry::Vacant(_) => unreachable!("Updating vacant L2 book entry"),
                btree_map::Entry::Occupied(mut o) => {
                    o.get_mut().remove_order(order.size());
                    if o.get().is_empty() {
                        o.remove();
                    }
                }
            },
        }
    }

    fn impact<'a>(
        mut side: impl Iterator<Item = (&'a UD64, &'a L2Level)>,
        want_size: UD64,
    ) -> Option<(UD64, UD64, UD64)> {
        let (price, unfilled, price_size) = side
            .fold_while(
                (UD64::ZERO, want_size, UD128::ZERO),
                |(_, unfilled, price_size), (price, level)| {
                    if unfilled > level.size {
                        FoldWhile::Continue((
                            *price,
                            unfilled - level.size,
                            price_size + (price.resize() * level.size.resize()),
                        ))
                    } else {
                        FoldWhile::Done((
                            *price,
                            UD64::ZERO,
                            price_size + (price.resize() * unfilled.resize()),
                        ))
                    }
                },
            )
            .into_inner();
        let filled = want_size - unfilled;
        if filled > UD64::ZERO {
            Some((price, filled, (price_size / filled.resize()).resize()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use fastnum::udec64;

    use super::*;

    #[test]
    fn test_l2_book() {
        let mut book = L2Book::new();

        book.add_order(&Order::for_testing(
            types::OrderType::OpenShort,
            udec64!(130),
            udec64!(0.3),
        ));
        book.add_order(&Order::for_testing(
            types::OrderType::OpenShort,
            udec64!(120),
            udec64!(0.2),
        ));
        book.add_order(&Order::for_testing(
            types::OrderType::OpenShort,
            udec64!(110),
            udec64!(0.1),
        ));

        book.add_order(&Order::for_testing(
            types::OrderType::OpenLong,
            udec64!(90),
            udec64!(0.2),
        ));
        book.add_order(&Order::for_testing(
            types::OrderType::OpenLong,
            udec64!(80),
            udec64!(0.3),
        ));
        book.add_order(&Order::for_testing(
            types::OrderType::OpenLong,
            udec64!(70),
            udec64!(0.4),
        ));

        assert_eq!(book.best_ask(), Some((udec64!(110), udec64!(0.1))));
        assert_eq!(book.best_bid(), Some((udec64!(90), udec64!(0.2))));

        assert_eq!(
            book.ask_impact(udec64!(0.05)),
            Some((udec64!(110), udec64!(0.05), udec64!(110)))
        );
        assert_eq!(
            book.ask_impact(udec64!(0.2)),
            Some((udec64!(120), udec64!(0.2), udec64!(115)))
        );
        assert_eq!(
            book.ask_impact(udec64!(0.3)),
            Some((udec64!(120), udec64!(0.3), udec64!(35) / udec64!(0.3)))
        );
        assert_eq!(
            book.ask_impact(udec64!(0.6)),
            Some((udec64!(130), udec64!(0.6), udec64!(74) / udec64!(0.6)))
        );
        assert_eq!(
            book.ask_impact(udec64!(1)),
            Some((udec64!(130), udec64!(0.6), udec64!(74) / udec64!(0.6)))
        );

        assert_eq!(
            book.bid_impact(udec64!(0.05)),
            Some((udec64!(90), udec64!(0.05), udec64!(90)))
        );
        assert_eq!(
            book.bid_impact(udec64!(0.3)),
            Some((udec64!(80), udec64!(0.3), udec64!(26) / udec64!(0.3)))
        );
        assert_eq!(
            book.bid_impact(udec64!(0.5)),
            Some((udec64!(80), udec64!(0.5), udec64!(42) / udec64!(0.5)))
        );
        assert_eq!(
            book.bid_impact(udec64!(0.9)),
            Some((udec64!(70), udec64!(0.9), udec64!(70) / udec64!(0.9)))
        );
        assert_eq!(
            book.bid_impact(udec64!(1)),
            Some((udec64!(70), udec64!(0.9), udec64!(70) / udec64!(0.9)))
        );

        book.update_order(
            &Order::for_testing(types::OrderType::OpenShort, udec64!(110), udec64!(0.05)),
            udec64!(0.1),
        );
        assert_eq!(book.best_ask(), Some((udec64!(110), udec64!(0.05))));

        book.update_order(
            &Order::for_testing(types::OrderType::OpenLong, udec64!(90), udec64!(0.3)),
            udec64!(0.2),
        );
        assert_eq!(book.best_bid(), Some((udec64!(90), udec64!(0.3))));

        book.remove_order(&Order::for_testing(
            types::OrderType::OpenShort,
            udec64!(110),
            udec64!(0.05),
        ));
        assert_eq!(book.best_ask(), Some((udec64!(120), udec64!(0.2))));

        book.remove_order(&Order::for_testing(
            types::OrderType::OpenLong,
            udec64!(90),
            udec64!(0.3),
        ));
        assert_eq!(book.best_bid(), Some((udec64!(80), udec64!(0.3))));
    }
}
