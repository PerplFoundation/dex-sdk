use std::collections::HashMap;

use alloy::primitives::{I256, TxHash, U256};
use fastnum::{UD64, udec128};

use crate::{
    Chain,
    abi::dex::Exchange::{
        AccountCreated, ContractAddedV2, ExchangeEvents, MaintenanceMarginFractionUpdated,
        MakerOrderFilled, OrderPlaced, PositionClosed, PositionOpened, RecycleFeeToAccount,
    },
    num::Converter,
    state::{
        ContractFeatures, Exchange, FeeSchedule, FeeScheduleKey, FeeScheduleRegistry, OrderContext,
        Perpetual,
    },
    stream::RawEvent,
    types::{
        self, OrderId, RequestId,
        RequestType::{self, CloseLong},
        StateInstant,
    },
};

const TEST_PERP_ID: u32 = 123456789;
/// A contract the test chain is configured never to track.
const EXCLUDED_PERP_ID: u32 = 30;
/// A contract listed while the test runs, which nothing excludes.
const LISTED_PERP_ID: u32 = 31;

fn create_test_exchange() -> Exchange { create_test_exchange_on(Chain::testnet()) }

fn create_test_exchange_on(chain: Chain) -> Exchange {
    let instant = StateInstant::new(0, 0);
    let collateral_converter = Converter::new(4);

    let perpetuals = HashMap::from([(TEST_PERP_ID, Perpetual::for_testing(TEST_PERP_ID))]);
    let accounts = HashMap::new();

    Exchange::new(
        chain,
        instant,
        ContractFeatures::current(),
        collateral_converter,
        100,
        udec128!(0.001),
        udec128!(0.001),
        udec128!(0.001),
        FeeScheduleRegistry::new(
            FeeSchedule::flat(FeeScheduleKey::Default, UD64::ZERO, UD64::ZERO),
            FeeSchedule::flat(FeeScheduleKey::RwaDefault, UD64::ZERO, UD64::ZERO),
            HashMap::new(),
        ),
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
        builder: None,
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

fn smart_contract_position_closed_inner(request_id: RequestId) -> (Exchange, Option<OrderContext>) {
    let mut exchange = create_test_exchange();

    let mut order_context =
        Some(create_test_order_context(request_id, None, 1, CloseLong, U256::from(123)));

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

/// A listing of `perp_id` on the exchange-wide default fee schedule. Only the
/// identity and the decimals matter here; the rest of the parameters are left
/// at zero.
fn event_contract_added(perp_id: u32) -> ExchangeEvents {
    ExchangeEvents::ContractAddedV2(ContractAddedV2 {
        perpId: U256::from(perp_id),
        name: format!("PERP{perp_id}"),
        symbol: format!("P{perp_id}"),
        status: 0,
        basePricePNS: U256::ZERO,
        priceDecimals: U256::from(2),
        lotDecimals: U256::from(2),
        initMarginFracHdths: U256::ZERO,
        maintMarginFracHdths: U256::ZERO,
        maxOpenInterestLNS: U256::ZERO,
        unityDescentThreshHdths: U256::ZERO,
        overColDescentThreshHdths: U256::ZERO,
        dcpBorrowThreshHdths: U256::ZERO,
        priceTolPer100K: U256::ZERO,
        marginTol: U256::ZERO,
        marginTolDecimals: U256::ZERO,
        refPriceMaxAgeSec: U256::ZERO,
        absFundingClampPctPer100K: U256::ZERO,
        permCancelMinOrders: U256::ZERO,
        permCancelSegment: U256::ZERO,
        insAmtPer100K: U256::ZERO,
        liqInsAmtPer100K: U256::ZERO,
        liqUserAmtPer100K: U256::ZERO,
        btlRestrictBuyers: false,
        btlPriceThreshPer100K: U256::ZERO,
        btlInsAmtPer100K: U256::ZERO,
        btlUserAmtPer100K: U256::ZERO,
        btlBuyerAmtPer100K: U256::ZERO,
        numPerpetuals: U256::ZERO,
        perpFeeSchedId: FeeScheduleKey::Default.to_raw(),
    })
}

fn event_maker_order_filled_on(perp_id: u32, account_id: u64, balance_cns: u64) -> ExchangeEvents {
    ExchangeEvents::MakerOrderFilled(MakerOrderFilled {
        perpId: U256::from(perp_id),
        accountId: U256::from(account_id),
        orderId: U256::from(1),
        pricePNS: U256::ZERO,
        lotLNS: U256::ZERO,
        feeCNS: U256::ZERO,
        lockedBalanceCNS: U256::ZERO,
        amountCNS: I256::ZERO,
        balanceCNS: U256::from(balance_cns),
    })
}

/// An excluded contract stays untracked when the block that LISTS it is
/// indexed, which is the only way it could re-enter the tracked set after
/// discovery left it out.
#[test]
fn test_excluded_perpetual_is_not_tracked_when_listed() {
    let chain = Chain::testnet().with_excluded_perpetuals(vec![EXCLUDED_PERP_ID]);
    let mut exchange = create_test_exchange_on(chain);
    let mut order_context = None;

    apply_event(&mut exchange, event_contract_added(EXCLUDED_PERP_ID), &mut order_context, 0);
    apply_event(&mut exchange, event_contract_added(LISTED_PERP_ID), &mut order_context, 1);

    assert!(
        !exchange.perpetuals().contains_key(&EXCLUDED_PERP_ID),
        "an excluded contract must not be tracked even once its listing is indexed"
    );
    assert!(
        exchange.perpetuals().contains_key(&LISTED_PERP_ID),
        "a listing that nothing excludes must still be tracked"
    );
}

/// Excluding a contract drops its own state, not the account state that its
/// events happen to carry: a fill reports the account's exchange-wide balance,
/// and an exclusion that swallowed it would freeze the balance of every account
/// trading that contract.
#[test]
fn test_excluded_perpetual_still_applies_account_balance() {
    let chain = Chain::testnet().with_excluded_perpetuals(vec![EXCLUDED_PERP_ID]);
    let mut exchange = create_test_exchange_on(chain);
    let mut order_context = None;

    apply_event(&mut exchange, event_account_created(1), &mut order_context, 0);
    apply_event(&mut exchange, event_contract_added(EXCLUDED_PERP_ID), &mut order_context, 1);
    apply_event(
        &mut exchange,
        event_maker_order_filled_on(EXCLUDED_PERP_ID, 1, 50_000),
        &mut order_context,
        2,
    );

    // Converter::new(4): 50_000 collateral units is 5.0.
    assert_eq!(
        exchange.accounts().get(&1).expect("UT").balance(),
        udec128!(5.0),
        "a fill on an excluded contract must still apply the account balance it reports"
    );
}

#[test]
fn test_smart_contract_position_closed() {
    let maker_client_order_id = 42;
    let (mut exchange, mut order_context) =
        smart_contract_position_closed_inner(maker_client_order_id);

    let maker_order_filled = event_maker_order_filled(1, 1);
    apply_event(&mut exchange, maker_order_filled, &mut order_context, 5);

    let maker_fill = order_context
        .as_ref()
        .and_then(|context| context.maker_fills.first())
        .expect("maker fill exists");
    assert_eq!(maker_fill.maker_client_order_id, Some(maker_client_order_id));

    // PositionClosed -> MakerOrderFilled implies Close Position
    let perps = exchange.perpetuals();
    let perp = perps.get(&TEST_PERP_ID).expect("UT");
    assert!(perp.get_order(OrderId::new(1).expect("UT")).is_none());
}

#[test]
fn test_smart_contract_position_closed_recycling_fee_to_account() {
    let (mut exchange, mut order_context) = smart_contract_position_closed_inner(1);

    let recycle_fee_to_account = event_recycle_fee_to_account(1, 1, 1, 1);
    apply_event(&mut exchange, recycle_fee_to_account, &mut order_context, 5);

    let maker_order_filled = event_maker_order_filled(1, 1);
    apply_event(&mut exchange, maker_order_filled, &mut order_context, 6);

    // PositionClosed -> Any -> MakerOrderFilled implies Close Position
    let perps = exchange.perpetuals();
    let perp = perps.get(&TEST_PERP_ID).expect("UT");
    assert!(perp.get_order(OrderId::new(1).expect("UT")).is_none());
}
