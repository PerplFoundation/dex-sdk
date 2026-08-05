//! Validates the smart-contract v1.1.7.4 additions across the snapshot and the
//! live event stream:
//!
//! - **Contract version** (`getContractVersion`) drives feature detection.
//! - **Perpetual discovery** (`getPerpetualExistsBitmap`) replaces a configured
//!   perpetual list.
//! - **Keyed, tiered fee schedules** - the exchange-wide default and RWA
//!   schedules, a per-contract custom one, and the repoints between them, via
//!   both `getPerpFeeSchedule` and the `*FeeScheduleSet` / `PerpFeeKeySet`
//!   events.
//! - **Per-account fee tiers** (`getAccountFeeTier` / `AccountFeeTierSet`).
//! - **Builder attribution** carried by the V2 order entrypoints' extension
//!   envelope, recovered from `getOrderV2` on the snapshot path and from
//!   `OrderRequestV2` on the streaming path, with the fee earned per fill
//!   reported by `MakerOrderFilledV2` / `TakerOrderFilledV2`.

use fastnum::{UD64, udec64, udec128};
use perpl_sdk::{
    state::{self, SnapshotBuilder},
    testing,
    types::{
        self, BuilderAttribution,
        RequestType::{OpenLong, OpenShort},
    },
};

/// Builder of the resting maker order, charging 0.0005 (50 Per100K).
const MAKER_BUILDER: types::BuilderId = 7;
const MAKER_BUILDER_FEE: UD64 = udec64!(0.0005);

/// Builder of the taker order, charging 0.001 (100 Per100K).
const TAKER_BUILDER: types::BuilderId = 9;
const TAKER_BUILDER_FEE: UD64 = udec64!(0.001);

/// Fee tier the taker account is moved to before trading.
const TAKER_FEE_TIER: types::FeeTier = 2;

/// Eight taker rates, one per fee tier, descending from the base rate.
const TAKER_FEES: [UD64; state::FEE_TIERS] = [
    udec64!(0.00069),
    udec64!(0.0006),
    udec64!(0.0005),
    udec64!(0.0004),
    udec64!(0.0003),
    udec64!(0.0002),
    udec64!(0.0001),
    udec64!(0),
];

/// Eight maker rates, one per fee tier, descending from the base rate.
const MAKER_FEES: [UD64; state::FEE_TIERS] = [
    udec64!(0.00009),
    udec64!(0.00008),
    udec64!(0.00007),
    udec64!(0.00006),
    udec64!(0.00005),
    udec64!(0.00004),
    udec64!(0.00003),
    udec64!(0),
];

#[tokio::test]
async fn test_sc_v1174() {
    let exchange = testing::TestExchange::new().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;
    let btc_perp = exchange.btc_perp().await;
    let eth_perp = exchange.eth_perp().await;

    // Tiered fees exchange-wide, with the taker on a discounted tier
    exchange.set_fee_schedule(TAKER_FEES, MAKER_FEES).await;
    exchange
        .set_account_fee_tiers(vec![(taker.id, TAKER_FEE_TIER)])
        .await;

    // A resting maker order attributed to a builder, so the snapshot has
    // attribution to recover from `getOrderV2`
    let maker_builder = BuilderAttribution::new(MAKER_BUILDER, MAKER_BUILDER_FEE);
    _ = btc_perp
        .order_v2(
            maker.id,
            order(1, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1))
                .with_builder(maker_builder),
        )
        .await
        .get_receipt()
        .await
        .unwrap();

    // ── snapshot path ───────────────────────────────────────────────────────

    // No perpetuals configured: the snapshot discovers every listed contract
    let discovered =
        SnapshotBuilder::new(&exchange.chain_with_perpetual_discovery(), exchange.provider.clone())
            .with_accounts(vec![
                types::AccountAddressOrID::ID(maker.id),
                types::AccountAddressOrID::ID(taker.id),
            ])
            .build()
            .await
            .unwrap();

    // `getContractVersion` is authoritative, so every v1.1.7.4 feature resolves
    // from it without further probing
    assert_eq!(discovered.contract_version(), Some(state::ContractVersion::BUILDER_CODES));
    let features = discovered.features();
    assert!(features.v2_state_getters());
    assert!(features.keyed_fee_schedules());
    assert!(features.builder_attribution());
    assert!(features.perpetual_discovery());

    // Both listed contracts found from the existence bitmap alone
    let mut discovered_ids: Vec<_> = discovered.perpetuals().keys().copied().collect();
    discovered_ids.sort();
    assert_eq!(discovered_ids, vec![btc_perp.id, eth_perp.id]);

    // The exchange-wide default schedule, tier by tier...
    let default_schedule = discovered.default_fee_schedule();
    assert_eq!(default_schedule.key(), state::FeeScheduleKey::Default);
    assert_eq!(default_schedule.taker_fees(), &TAKER_FEES);
    assert_eq!(default_schedule.maker_fees(), &MAKER_FEES);
    // ...which every contract points at until repointed
    for perp in discovered.perpetuals().values() {
        assert_eq!(perp.fee_schedule().key(), state::FeeScheduleKey::Default);
        assert_eq!(perp.taker_fee(), TAKER_FEES[0], "base rate is tier 0");
        assert_eq!(perp.taker_fee_for_tier(TAKER_FEE_TIER), TAKER_FEES[TAKER_FEE_TIER as usize]);
        assert_eq!(perp.maker_fee_for_tier(TAKER_FEE_TIER), MAKER_FEES[TAKER_FEE_TIER as usize]);
    }

    // Account fee tiers are snapshotted for explicitly requested accounts
    assert_eq!(discovered.accounts().get(&taker.id).unwrap().fee_tier(), Some(TAKER_FEE_TIER));
    assert_eq!(discovered.accounts().get(&maker.id).unwrap().fee_tier(), Some(0));

    // Builder attribution is persisted with the resting order, so unlike the
    // order flags it survives into the snapshot
    let snapshot_order = discovered
        .perpetuals()
        .get(&btc_perp.id)
        .unwrap()
        .l3_book()
        .ask_orders()
        .next()
        .expect("resting maker order")
        .builder()
        .expect("builder attribution recovered from getOrderV2");
    assert_eq!(snapshot_order.builder_id(), MAKER_BUILDER);
    assert_eq!(snapshot_order.fee(), MAKER_BUILDER_FEE);

    // ── streaming path ──────────────────────────────────────────────────────

    let (indexer, mut state) = testing::Indexer::new(&exchange).await;
    tokio::spawn(indexer.run(tokio::time::sleep));

    // Take the resting order with a differently attributed taker order
    let taker_builder = BuilderAttribution::new(TAKER_BUILDER, TAKER_BUILDER_FEE);
    _ = btc_perp
        .order_v2(
            taker.id,
            order(2, btc_perp.id, OpenLong, udec64!(100000), udec64!(0.1))
                .with_builder(taker_builder),
        )
        .await
        .get_receipt()
        .await
        .unwrap();

    // Give the perpetual its own fee schedule, move the taker to the base tier
    // and retune the exchange-wide default - three distinct fee events
    _ = btc_perp
        .set_fee_schedule([udec64!(0.001); state::FEE_TIERS], [udec64!(0.0002); state::FEE_TIERS])
        .await
        .get_receipt()
        .await
        .unwrap();
    exchange.set_account_fee_tiers(vec![(taker.id, 0)]).await;
    exchange
        .set_fee_schedule([udec64!(0.0009); state::FEE_TIERS], [udec64!(0.0001); state::FEE_TIERS])
        .await;
    // A trailing order request gives the loop below a request id to stop at
    _ = btc_perp
        .order_v2(maker.id, order(3, btc_perp.id, OpenShort, udec64!(100001), udec64!(0.1)))
        .await
        .get_receipt()
        .await
        .unwrap();

    let mut maker_fill_seen = false;
    let mut taker_fill_seen = false;
    let mut trade_seen = false;
    let mut fee_tier_seen = false;
    let mut perp_schedule_seen = false;
    let mut exchange_schedule_seen = false;
    let mut default_fanout_seen = false;
    while let Some(block_events) = state.next_state_events().await {
        for event in block_events.events().iter().flat_map(|e| e.event()) {
            match event {
                // Fees on the added size: the account pays its tier's rate plus
                // the builder's, and the builder's part is INCLUDED in the fee.
                state::StateEvents::Order(state::OrderEvent {
                    account_id,
                    builder: Some(builder),
                    r#type:
                        state::OrderEventType::Filled { fill_size, fee, builder_fee, is_maker, .. },
                    ..
                }) if *is_maker && *account_id == maker.id => {
                    assert_eq!(builder.builder_id(), MAKER_BUILDER);
                    assert_eq!(*fill_size, udec64!(0.1));
                    // 0.1 * 100000 * 0.0005
                    assert_eq!(*builder_fee, udec64!(5));
                    // ...plus the maker's tier-0 rate: 0.1 * 100000 * 0.00009
                    assert_eq!(*fee, udec64!(5.9));
                    maker_fill_seen = true;
                },
                state::StateEvents::Order(state::OrderEvent {
                    account_id,
                    builder: Some(builder),
                    r#type: state::OrderEventType::Filled { fee, builder_fee, is_maker, .. },
                    ..
                }) if !*is_maker && *account_id == taker.id => {
                    assert_eq!(builder.builder_id(), TAKER_BUILDER);
                    // 0.1 * 100000 * 0.001
                    assert_eq!(*builder_fee, udec64!(10));
                    // ...plus the taker's TIER-2 rate: 0.1 * 100000 * 0.0005
                    assert_eq!(*fee, udec64!(15));
                    taker_fill_seen = true;
                },

                state::StateEvents::Trade(trade) if trade.taker_request_id == 2 => {
                    assert_eq!(trade.taker_builder.map(|b| b.builder_id()), Some(TAKER_BUILDER));
                    assert_eq!(trade.taker_builder_fee, udec64!(10));
                    let fill = trade.maker_fills.first().expect("maker fill");
                    assert_eq!(fill.builder.map(|b| b.builder_id()), Some(MAKER_BUILDER));
                    assert_eq!(fill.builder_fee, udec64!(5));
                    assert_eq!(trade.total_builder_fees(), udec64!(15));
                    assert_eq!(trade.builder_total(MAKER_BUILDER), udec64!(5));
                    assert_eq!(trade.builder_total(TAKER_BUILDER), udec64!(10));
                    trade_seen = true;
                },

                state::StateEvents::Account(state::AccountEvent {
                    account_id,
                    r#type: state::AccountEventType::FeeTierUpdated(tier),
                    ..
                }) if *account_id == taker.id => {
                    assert_eq!(*tier, 0);
                    fee_tier_seen = true;
                },

                state::StateEvents::Perpetual(state::PerpetualEvent {
                    perpetual_id,
                    r#type: state::PerpetualEventType::FeeScheduleUpdated(schedule),
                }) => match schedule.key() {
                    // The contract's own schedule, keyed by its ID
                    state::FeeScheduleKey::Custom(id) if id == *perpetual_id => {
                        assert_eq!(*perpetual_id, btc_perp.id);
                        assert_eq!(schedule.base_taker_fee(), udec64!(0.001));
                        assert_eq!(schedule.taker_fee(7), udec64!(0.001));
                        perp_schedule_seen = true;
                    },
                    // Retuning the exchange-wide default fans out to the
                    // contracts still pointing at it - and only those
                    state::FeeScheduleKey::Default => {
                        assert_eq!(*perpetual_id, eth_perp.id);
                        assert_eq!(schedule.base_taker_fee(), udec64!(0.0009));
                        default_fanout_seen = true;
                    },
                    _ => (),
                },

                state::StateEvents::Exchange(state::ExchangeEvent::FeeScheduleUpdated(
                    schedule,
                )) if schedule.key() == state::FeeScheduleKey::Default => {
                    assert_eq!(schedule.base_taker_fee(), udec64!(0.0009));
                    assert_eq!(schedule.base_maker_fee(), udec64!(0.0001));
                    exchange_schedule_seen = true;
                },

                _ => (),
            }
        }

        if state.request_id_seen(3) {
            break;
        }
    }
    assert!(maker_fill_seen, "maker fill with builder attribution not seen");
    assert!(taker_fill_seen, "taker fill with builder attribution not seen");
    assert!(trade_seen, "trade with builder attribution not seen");
    assert!(fee_tier_seen, "account fee tier update not seen");
    assert!(perp_schedule_seen, "per-contract fee schedule update not seen");
    assert!(exchange_schedule_seen, "exchange-wide fee schedule update not seen");
    assert!(default_fanout_seen, "default schedule fan-out not seen");

    // ── state kept up to date by the events above ───────────────────────────
    {
        let snapshot = state.snapshot().clone();

        // The taker moved back to the base tier
        assert_eq!(snapshot.accounts().get(&taker.id).unwrap().fee_tier(), Some(0));

        // BTC now on its own schedule, so the default retune left it alone
        let btc = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        assert_eq!(btc.fee_schedule().key(), state::FeeScheduleKey::Custom(btc_perp.id));
        assert_eq!(btc.taker_fee(), udec64!(0.001));
        // ...while ETH followed it
        let eth = snapshot.perpetuals().get(&eth_perp.id).unwrap();
        assert_eq!(eth.fee_schedule().key(), state::FeeScheduleKey::Default);
        assert_eq!(eth.taker_fee(), udec64!(0.0009));
        assert_eq!(snapshot.default_fee_schedule().base_taker_fee(), udec64!(0.0009));

        // 100000 balance, less the 1000 position deposit (10000 notional at 10x)
        // and the 15 taker-side fee
        assert_eq!(snapshot.accounts().get(&taker.id).unwrap().balance(), udec128!(98985));
        assert!(snapshot.accounts().get(&maker.id).unwrap().balance() < udec128!(1000000));
    }
}

/// A batched V2 request whose builder-code envelope the contract cannot make
/// sense of is skipped rather than reverting the whole batch, and the SDK
/// surfaces the skip as an order error against the request that carried it.
///
/// The SDK never *produces* such an envelope -
/// [`BuilderAttribution::try_encode`] rejects out-of-range attribution up front
/// - so the envelope here is hand-rolled with an unsupported version tag, the
/// way a third-party submitter could.
#[tokio::test]
async fn test_order_extension_rejected() {
    use alloy::{
        primitives::{Bytes, U256},
        sol_types::SolValue,
    };

    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc_perp = exchange.btc_perp().await;

    let (indexer, mut state) = testing::Indexer::new(&exchange).await;
    tokio::spawn(indexer.run(tokio::time::sleep));

    // Two orders, the first carrying an envelope tagged with a version the
    // contract's decoder does not know
    let good = order(1, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1));
    let rejected = order(2, btc_perp.id, OpenShort, udec64!(100001), udec64!(0.1));
    let unknown_version: Bytes =
        (7u16, Bytes::from((U256::from(1), U256::from(10)).abi_encode_params()))
            .abi_encode_params()
            .into();

    let snapshot = state.snapshot().clone();
    let descs =
        vec![rejected.prepare_v2(&snapshot).unwrap().0, good.prepare_v2(&snapshot).unwrap().0];
    _ = exchange
        .exchange
        .execOrdersV2(descs, false, vec![unknown_version, Bytes::new()])
        .from(trader.address)
        .gas(150_000_000)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    let mut rejection_seen = false;
    while let Some(block_events) = state.next_state_events().await {
        for event in block_events.events().iter().flat_map(|e| e.event()) {
            if let state::StateEvents::Error(state::OrderError {
                request_id: 2,
                r#type: state::OrderErrorType::OrderExtensionRejected,
                ..
            }) = event
            {
                rejection_seen = true;
            }
        }
        if state.request_id_seen(1) {
            break;
        }
    }
    assert!(rejection_seen, "rejected order extension not reported");

    // Only the order with the sound envelope reached the book
    let snapshot = state.snapshot().clone();
    let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
    assert_eq!(perp.total_orders(), 1);
    assert_eq!(
        perp.l3_book()
            .ask_orders()
            .next()
            .unwrap()
            .client_order_id(),
        Some(1),
        "the skipped order must not be in the book",
    );
}

/// An order request with the boilerplate parameters filled in.
fn order(
    request_id: types::RequestId,
    perp_id: types::PerpetualId,
    r#type: types::RequestType,
    price: UD64,
    size: UD64,
) -> types::OrderRequest {
    types::OrderRequest::new(
        request_id,
        perp_id,
        r#type,
        None,
        price,
        size,
        None,
        false,
        false,
        false,
        None,
        udec64!(10),
        None,
        None,
        1000,
    )
}
