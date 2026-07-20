//! End-to-end `Exchange::apply_events` tests for the funding-before-decrease fix (Bug 44).
//!
//! Unlike the Position/Perpetual unit tests (which hand-call the two steps in order), these drive
//! the real three-pass `apply_events` wiring, so they are the tests that FAIL if the passes are
//! reordered. They are fast in-crate synthetic-event tests — no Anvil, no RPC.
//!
//! Convention for clean arithmetic: the test perpetual uses `Converter::new(0)` for price/size/
//! funding (values pass through) and `Converter::new(4)` for collateral; entry = 100, size = 10,
//! maintenance margin = 20 (so MMR = entry*size/mm = 50), funding payment = 1 per unit.

use std::collections::HashMap;

use alloy::primitives::{I256, TxHash, U256};
use fastnum::{D256, dec256, dec64, udec64, udec128};

use crate::{
    Chain,
    abi::dex::Exchange::{
        AccountCreated, ExchangeEvents, MaintenanceMarginFractionUpdated, PositionDecreased,
        PositionOpened,
    },
    num::Converter,
    state::{Exchange, Perpetual},
    stream::{RawBlockEvents, RawEvent},
    types::StateInstant,
};

const PERP: u32 = 123456789;
const PERP_B: u32 = 987654321;
const LONG: u8 = 0;
const SHORT: u8 = 1;

// ── builders ────────────────────────────────────────────────────────────────────────────────

fn si(block: u64) -> StateInstant { StateInstant::new(block, block) }

fn ev(event: ExchangeEvents, log_index: u64) -> RawEvent {
    RawEvent::new(TxHash::ZERO, 0, log_index, event)
}

/// A test perpetual with a nonzero maintenance margin (mm = 20), so `Position::opened` — which
/// computes MMR = entry*size/mm — does not divide by zero. Converters are the `for_testing`
/// defaults (price/size/funding scale 0).
fn perp(id: u32) -> Perpetual {
    let mut p = Perpetual::for_testing(id);
    p.update_maintenance_margin(si(0), udec64!(20));
    p
}

/// A test perpetual with funding of `payment` per unit scheduled to take effect at `event_block`.
/// `update_funding` is exactly the call the `FundingEventCompleted` handler makes.
fn perp_funding(id: u32, payment: D256, event_block: u64) -> Perpetual {
    let mut p = perp(id);
    p.update_funding(si(0), dec64!(0.01), payment, event_block);
    p
}

fn exchange(perps: HashMap<u32, Perpetual>) -> Exchange {
    Exchange::new(
        Chain::testnet(),
        si(0),
        Converter::new(4), // collateral converter
        100,
        udec128!(0.001),
        udec128!(0.001),
        udec128!(0.001),
        perps,
        HashMap::new(),
        false, // is_halted
        true,  // track_all_accounts
    )
}

fn account_created(id: u32) -> ExchangeEvents {
    ExchangeEvents::AccountCreated(AccountCreated { account: Default::default(), id: U256::from(id) })
}

fn position_opened(perp_id: u32, account: u32, kind: u8, price: u64, lot: u64, deposit: u64) -> ExchangeEvents {
    ExchangeEvents::PositionOpened(PositionOpened {
        perpId: U256::from(perp_id),
        accountId: U256::from(account),
        positionType: kind,
        leverageHdths: U256::ZERO,
        depositCNS: U256::from(deposit),
        pnlCollateralizedCNS: I256::ZERO,
        pricePNS: U256::from(price),
        lotLNS: U256::from(lot),
        insFeeCNS: U256::ZERO,
        protFeeCNS: U256::ZERO,
    })
}

fn position_decreased(perp_id: u32, account: u32, kind: u8, end_lot: u64, funding_cns: i64) -> ExchangeEvents {
    ExchangeEvents::PositionDecreased(PositionDecreased {
        perpId: U256::from(perp_id),
        accountId: U256::from(account),
        positionType: kind,
        startDepositCNS: U256::ZERO,
        endDepositCNS: U256::from(deposit_for(end_lot)),
        startLotLNS: U256::from(10u64),
        endLotLNS: U256::from(end_lot),
        deltaPnlCNS: I256::ZERO,
        fundingCNS: I256::try_from(funding_cns).unwrap(),
    })
}

fn maintenance_margin(perp_id: u32, hdths: u64) -> ExchangeEvents {
    ExchangeEvents::MaintenanceMarginFractionUpdated(MaintenanceMarginFractionUpdated {
        perpId: U256::from(perp_id),
        maintMarginFracHdths: U256::from(hdths),
    })
}

fn deposit_for(lot: u64) -> u64 { lot * 100_000 }

// ── accessors ───────────────────────────────────────────────────────────────────────────────

fn premium(exchange: &Exchange, account: u32, perp_id: u32) -> D256 {
    exchange
        .accounts()
        .get(&account)
        .expect("account tracked")
        .positions()
        .get(&perp_id)
        .expect("position exists")
        .premium_pnl()
}

// ── tests ───────────────────────────────────────────────────────────────────────────────────

/// T1 — the flagship regression. A block whose funding takes effect AND that decreases positions
/// must apply the funding on each position's PRE-decrease size (Pass 1 before Pass 2). A
/// pass-reorder would apply it on the smaller post-decrease size. Covers both sides + an untouched
/// position, and is the only test that fails if the passes are reordered.
#[test]
fn exchange_funding_before_same_block_decrease_both_sides() {
    let perps = HashMap::from([(PERP, perp_funding(PERP, dec256!(1), 2))]);
    let mut exchange = exchange(perps);

    // Block 1: open Long (1), Short (2), and an untouched Long (3), each at price 100, size 10.
    exchange
        .apply_events(&RawBlockEvents::new(si(1), vec![
            ev(account_created(1), 0),
            ev(account_created(2), 1),
            ev(account_created(3), 2),
            ev(position_opened(PERP, 1, LONG, 100, 10, deposit_for(10)), 3),
            ev(position_opened(PERP, 2, SHORT, 100, 10, deposit_for(10)), 4),
            ev(position_opened(PERP, 3, LONG, 100, 10, deposit_for(10)), 5),
        ]))
        .expect("block 1");

    // Block 2 = the funding-event block: funding takes effect (Pass 1) AND accounts 1 & 2 decrease
    // 10 -> 4 (Pass 2). fundingCNS = 0 isolates the tick from the closed-portion realization.
    exchange
        .apply_events(&RawBlockEvents::new(si(2), vec![
            ev(position_decreased(PERP, 1, LONG, 4, 0), 0),
            ev(position_decreased(PERP, 2, SHORT, 4, 0), 1),
        ]))
        .expect("block 2");

    // Funding landed on the PRE-decrease size 10: long -1*1*10 = -10, short +1*1*10 = +10.
    // A pass-reorder would give -4 / +4 (funding on the post-decrease size 4).
    assert_eq!(premium(&exchange, 1, PERP), dec256!(-10), "long: funding on pre-decrease size");
    assert_eq!(premium(&exchange, 2, PERP), dec256!(10), "short: funding on pre-decrease size");
    // The untouched position still receives the block's tick.
    assert_eq!(premium(&exchange, 3, PERP), dec256!(-10), "untouched long still funded");
}

/// T2 — a position OPENED in the funding-event block receives no funding for that block: it is
/// inserted in Pass 2, so it is absent during Pass 1. A pre-existing position on the same perp
/// still gets the tick.
#[test]
fn exchange_position_opened_in_funding_block_gets_no_funding() {
    let perps = HashMap::from([(PERP, perp_funding(PERP, dec256!(1), 2))]);
    let mut exchange = exchange(perps);

    // Block 1: a pre-existing Long (account 1).
    exchange
        .apply_events(&RawBlockEvents::new(si(1), vec![
            ev(account_created(1), 0),
            ev(position_opened(PERP, 1, LONG, 100, 10, deposit_for(10)), 1),
        ]))
        .expect("block 1");

    // Block 2 = the funding-event block: funding (Pass 1) AND a NEW position opened (Pass 2).
    exchange
        .apply_events(&RawBlockEvents::new(si(2), vec![
            ev(account_created(2), 0),
            ev(position_opened(PERP, 2, LONG, 100, 10, deposit_for(10)), 1),
        ]))
        .expect("block 2");

    // Pre-existing position was funded on its pre-event size; the new one has premium 0.
    assert_eq!(premium(&exchange, 1, PERP), dec256!(-10), "pre-existing long funded");
    assert_eq!(premium(&exchange, 2, PERP), dec256!(0), "opened-in-block long unfunded");
}

/// T3 — multi-perp fan-out isolation: only positions on the perp whose funding is due accrue.
/// Guards the real `perpetuals.values_mut().filter_map(take_funding_payment)` selection.
#[test]
fn exchange_multi_perp_funding_fanout_isolation() {
    // PERP has funding due at block 2; PERP_B has none.
    let perps = HashMap::from([
        (PERP, perp_funding(PERP, dec256!(1), 2)),
        (PERP_B, perp(PERP_B)),
    ]);
    let mut exchange = exchange(perps);

    // Block 1: account 1 holds a Long on BOTH perps.
    exchange
        .apply_events(&RawBlockEvents::new(si(1), vec![
            ev(account_created(1), 0),
            ev(position_opened(PERP, 1, LONG, 100, 10, deposit_for(10)), 1),
            ev(position_opened(PERP_B, 1, LONG, 100, 10, deposit_for(10)), 2),
        ]))
        .expect("block 1");

    // Block 2 = PERP's funding-event block (no raw events; Pass 1 fans out to PERP only).
    exchange.apply_events(&RawBlockEvents::new(si(2), vec![])).expect("block 2");

    assert_eq!(premium(&exchange, 1, PERP), dec256!(-10), "PERP position funded");
    assert_eq!(premium(&exchange, 1, PERP_B), dec256!(0), "PERP_B position untouched");
}

/// T4 — Pass-1 funding vs Pass-3 maintenance-margin fan-out compose in one block: funding lands in
/// Pass 1, the MMF change is applied to the perp in Pass 2 and fanned out to positions in Pass 3,
/// and the resulting liquidation price reflects both.
#[test]
fn exchange_pass1_funding_vs_pass3_mmf_ordering() {
    let perps = HashMap::from([(PERP, perp_funding(PERP, dec256!(1), 2))]);
    let mut exchange = exchange(perps);

    // Block 1: a Long (account 1). entry 100, size 10, deposit 100, mm 20 -> MMR 50, liq 95.
    exchange
        .apply_events(&RawBlockEvents::new(si(1), vec![
            ev(account_created(1), 0),
            ev(position_opened(PERP, 1, LONG, 100, 10, deposit_for(10)), 1),
        ]))
        .expect("block 1");
    assert_eq!(
        exchange.accounts().get(&1).unwrap().positions().get(&PERP).unwrap().liquidation_price(),
        udec64!(95), // 100 + (50 - 100 - 0)/10
    );

    // Block 2: funding effective (Pass 1: premium -10) AND MMF raised to hdths=1000 -> mm 10
    // (Pass 2 sets the perp param, Pass 3 recomputes MMR = 100*10/10 = 100 on every position).
    exchange
        .apply_events(&RawBlockEvents::new(si(2), vec![
            ev(maintenance_margin(PERP, 1000), 0),
        ]))
        .expect("block 2");

    let pos = exchange.accounts().get(&1).unwrap().positions().get(&PERP).unwrap().clone();
    assert_eq!(pos.premium_pnl(), dec256!(-10), "funding applied in Pass 1");
    assert_eq!(pos.maintenance_margin_requirement(), udec128!(100), "MMF fanned out in Pass 3");
    // liq = entry + (MMR - deposit - premium)/size = 100 + (100 - 100 - (-10))/10 = 101.
    assert_eq!(pos.liquidation_price(), udec64!(101), "liq composes funding + MMF");
}
