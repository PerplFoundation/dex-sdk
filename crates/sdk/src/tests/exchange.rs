use std::collections::HashMap;

use alloy::primitives::{I256, TxHash, U256};
use fastnum::udec128;

use crate::{
    Chain,
    abi::dex::Exchange::{
        AccountCreated, ExchangeEvents, MaintenanceMarginFractionUpdated, MakerOrderFilled,
        OrderPlaced, PositionClosed, PositionOpened, RecycleFeeToAccount,
    },
    num::Converter,
    state::{Exchange, OrderContext, Perpetual},
    stream::RawEvent,
    types::{
        self, OrderId, RequestId,
        RequestType::{self, CloseLong},
        StateInstant,
    },
};

const TEST_PERP_ID: u32 = 123456789;

fn create_test_exchange() -> Exchange {
    let chain = Chain::testnet();
    let instant = StateInstant::new(0, 0);
    let collateral_converter = Converter::new(4);

    let perpetuals = HashMap::from([(TEST_PERP_ID, Perpetual::for_testing(TEST_PERP_ID))]);
    let accounts = HashMap::new();

    Exchange::new(
        chain,
        instant,
        collateral_converter,
        100,
        udec128!(0.001),
        udec128!(0.001),
        udec128!(0.001),
        perpetuals,
        accounts,
        false,
        true,
    )
}

fn create_test_order_context(
    request_id: RequestId,
    order_id_opt: Option<OrderId>,
    account_id: types::AccountId,
    request_type: RequestType,
    price: U256,
) -> OrderContext {
    OrderContext {
        perpetual_id: TEST_PERP_ID,
        account_id,
        request_id,
        order_id: order_id_opt,
        r#type: request_type,
        price,
        expiry_block: 100_000_000,
        leverage: U256::from(5),
        post_only: false,
        fill_or_kill: false,
        immediate_or_cancel: false,
        maker_fills: vec![],
        clearing_remaining_order: false,
        position_closed_at_log_index: None,
    }
}

fn event_account_created(id: u64) -> ExchangeEvents {
    ExchangeEvents::AccountCreated(AccountCreated {
        account: Default::default(),
        id: U256::from(id),
    })
}

fn event_maintenance_margin(margin_fraction_hdths: u64) -> ExchangeEvents {
    ExchangeEvents::MaintenanceMarginFractionUpdated(MaintenanceMarginFractionUpdated {
        perpId: U256::from(TEST_PERP_ID),
        maintMarginFracHdths: U256::from(margin_fraction_hdths),
    })
}

fn event_order_placed(order_id: u64) -> ExchangeEvents {
    ExchangeEvents::OrderPlaced(OrderPlaced {
        orderId: U256::from(order_id),
        lotLNS: U256::from(1),
        lockedBalanceCNS: U256::ZERO,
        amountCNS: I256::ZERO,
        balanceCNS: U256::ZERO,
    })
}

fn event_position_opened(account_id: u64) -> ExchangeEvents {
    ExchangeEvents::PositionOpened(PositionOpened {
        perpId: U256::from(TEST_PERP_ID),
        accountId: U256::from(account_id),
        positionType: 0,
        leverageHdths: U256::ZERO,
        depositCNS: U256::ZERO,
        pnlCollateralizedCNS: "1".parse().unwrap(),
        pricePNS: U256::ZERO,
        lotLNS: U256::ZERO,
        insFeeCNS: U256::ZERO,
        protFeeCNS: U256::ZERO,
    })
}

fn event_position_closed(account_id: u64) -> ExchangeEvents {
    ExchangeEvents::PositionClosed(PositionClosed {
        perpId: U256::from(TEST_PERP_ID),
        accountId: U256::from(account_id),
        positionType: 0,
        pricePNS: U256::ZERO,
        deltaPnlCNS: I256::ZERO,
        fundingCNS: I256::ZERO,
    })
}

fn event_maker_order_filled(account_id: u64, order_id: u64) -> ExchangeEvents {
    ExchangeEvents::MakerOrderFilled(MakerOrderFilled {
        perpId: U256::from(TEST_PERP_ID),
        accountId: U256::from(account_id),
        orderId: U256::from(order_id),
        pricePNS: U256::ZERO,
        lotLNS: U256::ZERO,
        feeCNS: U256::ZERO,
        lockedBalanceCNS: U256::ZERO,
        amountCNS: I256::ZERO,
        balanceCNS: U256::ZERO,
    })
}

fn event_recycle_fee_to_account(
    account_id: u64,
    order_id: u64,
    recycle_fee: u64,
    recycle_balance: u64,
) -> ExchangeEvents {
    ExchangeEvents::RecycleFeeToAccount(RecycleFeeToAccount {
        accountId: U256::from(account_id),
        perpId: U256::from(TEST_PERP_ID),
        orderId: U256::from(order_id),
        recycleFeeCNS: U256::from(recycle_fee),
        recycleBalanceCNS: U256::from(recycle_balance),
    })
}

fn apply_event(
    exchange: &mut Exchange,
    exchange_event: ExchangeEvents,
    order_context: &mut Option<OrderContext>,
    log_index: u64,
) {
    let instant = StateInstant::new(0, 0);
    let raw_event = RawEvent::new(TxHash::ZERO, 0, log_index, exchange_event);
    exchange
        .apply_raw_event(instant, &raw_event, order_context)
        .expect("UT");
}

fn smart_contract_position_closed_inner() -> (Exchange, Option<OrderContext>) {
    let mut exchange = create_test_exchange();

    let mut order_context = Some(create_test_order_context(1, None, 1, CloseLong, U256::from(123)));

    let account_created = event_account_created(1);
    apply_event(&mut exchange, account_created, &mut order_context, 0);

    let maintenance_margin = event_maintenance_margin(1);
    apply_event(&mut exchange, maintenance_margin, &mut order_context, 1);

    let order_placed = event_order_placed(1);
    apply_event(&mut exchange, order_placed, &mut order_context, 2);

    let position_opened = event_position_opened(1);
    apply_event(&mut exchange, position_opened, &mut order_context, 3);

    let position_closed = event_position_closed(1);
    apply_event(&mut exchange, position_closed, &mut order_context, 4);

    let perps = exchange.perpetuals();
    let perp = perps.get(&TEST_PERP_ID).expect("UT");
    assert!(perp.get_order(OrderId::new(1).expect("UT")).is_some());
    (exchange, order_context)
}

#[test]
fn test_smart_contract_position_closed() {
    let (mut exchange, mut order_context) = smart_contract_position_closed_inner();

    let maker_order_filled = event_maker_order_filled(1, 1);
    apply_event(&mut exchange, maker_order_filled, &mut order_context, 5);

    // PositionClosed -> MakerOrderFilled implies Close Position
    let perps = exchange.perpetuals();
    let perp = perps.get(&TEST_PERP_ID).expect("UT");
    assert!(perp.get_order(OrderId::new(1).expect("UT")).is_none());
}

#[test]
fn test_smart_contract_position_closed_recycling_fee_to_account() {
    let (mut exchange, mut order_context) = smart_contract_position_closed_inner();

    let recycle_fee_to_account = event_recycle_fee_to_account(1, 1, 1, 1);
    apply_event(&mut exchange, recycle_fee_to_account, &mut order_context, 5);

    let maker_order_filled = event_maker_order_filled(1, 1);
    apply_event(&mut exchange, maker_order_filled, &mut order_context, 6);

    // PositionClosed -> Any -> MakerOrderFilled implies Close Position
    let perps = exchange.perpetuals();
    let perp = perps.get(&TEST_PERP_ID).expect("UT");
    assert!(perp.get_order(OrderId::new(1).expect("UT")).is_none());
}

#[cfg(feature = "dcp")]
fn apply_event_ret(
    exchange: &mut Exchange,
    exchange_event: ExchangeEvents,
    order_context: &mut Option<OrderContext>,
    log_index: u64,
) -> Vec<crate::state::StateEvents> {
    let instant = StateInstant::new(0, 0);
    let raw_event = RawEvent::new(TxHash::ZERO, 0, log_index, exchange_event);
    exchange
        .apply_raw_event(instant, &raw_event, order_context)
        .expect("UT")
}

// for_testing perps use price/size converters of scale 0, so PNS/LNS values pass
// through unscaled; the collateral converter is scale 4 (see create_test_exchange).
#[cfg(feature = "dcp")]
fn event_collateral_decrease_requested(account_id: u64) -> ExchangeEvents {
    ExchangeEvents::CollateralDecreaseRequested(
        crate::abi::dex::Exchange::CollateralDecreaseRequested {
            perpId: U256::from(TEST_PERP_ID),
            accountId: U256::from(account_id),
            expiryTS: U256::from(120),
            amountCNS: U256::ZERO,
            clampToMaximum: true,
            positionType: 0, // Long
            entryPricePNS: U256::from(100),
            lotLNS: U256::from(10),
        },
    )
}

#[cfg(feature = "dcp")]
#[test]
fn test_dcp_request_surfaced_and_mutates_nothing() {
    use fastnum::{udec64, udec128};

    use crate::state::{PositionEventType, PositionType};

    let mut exchange = create_test_exchange();
    let mut ctx: Option<OrderContext> = None;

    let out = apply_event_ret(&mut exchange, event_collateral_decrease_requested(1), &mut ctx, 0);

    assert_eq!(out.len(), 1, "request must surface exactly one notification");
    let pe = out[0].as_position_event().expect("expected a Position event");
    assert_eq!(pe.perpetual_id, TEST_PERP_ID);
    assert_eq!(pe.account_id, 1);
    match pe.r#type {
        PositionEventType::CollateralDecreaseRequested {
            expiry_ts,
            amount,
            clamp_to_maximum,
            r#type,
            entry_price,
            size,
        } => {
            assert_eq!(expiry_ts, 120);
            assert_eq!(amount, udec128!(0)); // amountCNS 0 == "withdraw max"
            assert!(clamp_to_maximum);
            assert_eq!(r#type, PositionType::Long);
            assert_eq!(entry_price, udec64!(100));
            assert_eq!(size, udec64!(10));
        },
        other => panic!("unexpected event type: {other:?}"),
    }

    // Pure notification: re-applying yields the identical event and no state drift
    // (open interest untouched — the arm calls no `update_*`).
    let out2 = apply_event_ret(&mut exchange, event_collateral_decrease_requested(1), &mut ctx, 1);
    assert_eq!(out2.len(), 1);
    let perp = exchange.perpetuals().get(&TEST_PERP_ID).expect("perp");
    assert_eq!(perp.open_interest(), udec128!(0));
}

#[cfg(feature = "dcp")]
#[test]
fn test_dcp_failure_event_surfaced() {
    use crate::state::{DcpFailureReason, PositionEventType};

    let mut exchange = create_test_exchange();
    let mut ctx: Option<OrderContext> = None;

    let ev = ExchangeEvents::BorrowMarginNotMetAfterDecCollateral(
        crate::abi::dex::Exchange::BorrowMarginNotMetAfterDecCollateral {
            perpId: U256::from(TEST_PERP_ID),
            accountId: U256::from(7),
            bmrCNS: I256::ZERO,
            fmvAfterCNS: I256::ZERO,
        },
    );
    let out = apply_event_ret(&mut exchange, ev, &mut ctx, 0);

    assert_eq!(out.len(), 1);
    let pe = out[0].as_position_event().expect("expected a Position event");
    assert_eq!(pe.account_id, 7);
    assert!(matches!(
        pe.r#type,
        PositionEventType::CollateralDecreaseFailed {
            reason: DcpFailureReason::BorrowMarginNotMet
        }
    ));
}
