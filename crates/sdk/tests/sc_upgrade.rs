//! Validates that a running indexer survives the v1.1.7.4 contract upgrade:
//! a snapshot taken against the previous generation, kept up to date across the
//! upgrade transaction itself, and then across trading on the upgraded
//! contract.
//!
//! This is the sequence a production indexer sees exactly once, and it is the
//! one the SDK cannot rehearse against either generation alone:
//!
//! - before the upgrade the deployed contract has no version getter, so the
//!   feature set is probed, fees are a flat per-contract pair, and orders and
//!   fills arrive as V1 events;
//! - the upgrade transaction rewrites the fee schedules and repoints every
//!   contract at the default one, and stamps the version **last** - so the fee
//!   events are consumed while the SDK still believes it is talking to the old
//!   generation;
//! - afterwards the same stream carries `OrderRequestV2` and the V2 fills, and
//!   builder attribution starts working.

use std::time::Duration;

use fastnum::{UD64, udec64, udec128};
use perpl_sdk::{
    abi::dex::Exchange::ExchangeEvents,
    state::{self, SnapshotBuilder},
    testing,
    types::{
        self, BuilderAttribution,
        RequestType::{OpenLong, OpenShort},
    },
};

/// Raw events of interest observed across the upgrade boundary.
#[derive(Debug, Default)]
struct RawEventCounts {
    order_request_v1: usize,
    order_request_v2: usize,
    maker_filled_v1: usize,
    maker_filled_v2: usize,
    contract_added_v1: usize,
    contract_added_v2: usize,
    version_set: usize,
}

impl RawEventCounts {
    /// Every event the boundary is expected to produce has been seen, so the
    /// stream can stop being drained - it never ends on its own.
    fn is_complete(&self) -> bool {
        self.order_request_v1 > 0
            && self.order_request_v2 > 0
            && self.maker_filled_v1 > 0
            && self.maker_filled_v2 > 0
            && self.contract_added_v1 > 0
            && self.contract_added_v2 > 0
            && self.version_set > 0
    }
}

/// Eight taker rates seeded by the upgrade, descending from the base rate.
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

/// Eight maker rates seeded by the upgrade, descending from the base rate.
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

const BUILDER: types::BuilderId = 11;
const BUILDER_FEE: UD64 = udec64!(0.001);

#[tokio::test]
async fn test_contract_upgrade_mid_stream() {
    // ── the previous contract generation ────────────────────────────────────
    let exchange = testing::TestExchange::new_at_previous_version().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;
    // The old generation carries fees per contract, set through setters this
    // release removed - a listing does not configure them on either generation
    let btc_perp = exchange
        .btc_perp()
        .await
        .with_legacy_fees(udec64!(0.00035), udec64!(0.00010))
        .await;
    let eth_perp = exchange
        .eth_perp()
        .await
        .with_legacy_fees(udec64!(0.00035), udec64!(0.00010))
        .await;

    // A snapshot of the old generation: no version to read, so the features it
    // introduced are absent and the perpetual set cannot be discovered from a
    // bitmap - it is probed instead.
    let (indexer, mut state) = testing::Indexer::new(&exchange).await;
    {
        let snapshot = state.snapshot().clone();
        assert_eq!(snapshot.contract_version(), None);
        assert!(snapshot.features().v2_state_getters(), "v1.1.7.3b has the V2 getters");
        assert!(!snapshot.features().keyed_fee_schedules());
        assert!(!snapshot.features().builder_attribution());
        assert!(!snapshot.features().perpetual_discovery());

        // Fees are a flat per-contract pair, standing in for a schedule
        let btc = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        assert_eq!(btc.fee_schedule().key(), state::FeeScheduleKey::Default);
        assert_eq!(btc.taker_fee(), udec64!(0.00035));
        assert_eq!(btc.taker_fee_for_tier(2), udec64!(0.00035), "no tiers to differ by");
        // ...so there are no tiers to report, and the detailed rendering says
        // nothing about them rather than repeating the base rate eight times
        assert!(!btc.fee_schedule().is_tiered());
        assert!(!format!("{btc:#}").contains("Fee tiers"));
        // ...and no account carries a fee tier, since the concept does not exist
        assert_eq!(snapshot.accounts().get(&taker.id).unwrap().fee_tier(), None);
    }

    // Discovery must also work without the bitmap, by probing the ID space
    let probed =
        SnapshotBuilder::new(&exchange.chain_with_perpetual_discovery(), exchange.provider.clone())
            .build()
            .await
            .unwrap();
    let mut probed_ids: Vec<_> = probed.perpetuals().keys().copied().collect();
    probed_ids.sort();
    assert_eq!(probed_ids, vec![btc_perp.id, eth_perp.id]);

    tokio::spawn(indexer.run(tokio::time::sleep));

    // Trade on the old generation: V1 order and fill events, no attribution
    _ = btc_perp
        .order(maker.id, order(1, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1)))
        .await
        .get_receipt()
        .await
        .unwrap();
    _ = btc_perp
        .order(taker.id, order(2, btc_perp.id, OpenLong, udec64!(100000), udec64!(0.1)))
        .await
        .get_receipt()
        .await
        .unwrap();

    // A builder-attributed order cannot even be prepared against this contract
    let builder = BuilderAttribution::new(BUILDER, BUILDER_FEE);
    let rejected = order(3, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1))
        .with_builder(builder)
        .prepare_v2(&state.snapshot().clone());
    assert!(
        matches!(rejected, Err(perpl_sdk::error::DexError::UnsupportedByContract(..))),
        "builder attribution must not be submitted to a contract that cannot carry it",
    );

    // A contract listed on the old generation announces itself with
    // `ContractAdded`, and its fees arrive separately through the deprecated
    // per-contract setters. The upgrade below must then fold it into the keyed fee
    // model along with the contracts that predate the stream.
    let trx_perp = exchange
        .trx_perp()
        .await
        .with_legacy_fees(udec64!(0.0005), udec64!(0.0002))
        .await;

    // ── the upgrade ─────────────────────────────────────────────────────────
    exchange.upgrade(TAKER_FEES, MAKER_FEES).await;

    // Trade again, now with attribution the upgraded contract does carry
    _ = btc_perp
        .order_v2(
            maker.id,
            order(10, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1)).with_builder(builder),
        )
        .await
        .get_receipt()
        .await
        .unwrap();
    _ = btc_perp
        .order_v2(taker.id, order(11, btc_perp.id, OpenLong, udec64!(100000), udec64!(0.1)))
        .await
        .get_receipt()
        .await
        .unwrap();

    // ── a contract listed AFTER the upgrade ─────────────────────────────────
    //
    // `ContractAddedV2` reports the contract's fee schedule KEY instead of
    // resolved base fees, and nothing else in the listing transaction reports the
    // key or the rates. So this is the case where the SDK must resolve fees for a
    // contract it learns about entirely from events, by pairing the key with a
    // schedule it is already tracking.
    let sol_perp = exchange.sol_perp().await;
    _ = sol_perp
        .order_v2(maker.id, order(20, sol_perp.id, OpenShort, udec64!(200), udec64!(0.5)))
        .await
        .get_receipt()
        .await
        .unwrap();
    _ = sol_perp
        .order_v2(taker.id, order(21, sol_perp.id, OpenLong, udec64!(200), udec64!(0.5)))
        .await
        .get_receipt()
        .await
        .unwrap();

    // ── what the stream carried across the boundary ─────────────────────────
    let mut v1_fill_seen = false;
    let mut version_seen = false;
    let mut default_schedule_seen = false;
    let mut rwa_schedule_seen = false;
    let mut repointed = vec![];
    let mut v2_fill_seen = false;
    let mut new_perp_fill_seen = false;
    let mut legacy_fee_seen = false;
    // Listings, each tagged with whether the upgrade had already been observed -
    // `ContractVersionSet` is the last log of the upgrade transaction, so it
    // separates the two listing generations exactly
    let mut listings: Vec<(types::PerpetualId, bool)> = vec![];
    while let Some(block_events) = state.next_state_events().await {
        for event in block_events.events().iter().flat_map(|e| e.event()) {
            match event {
                // Pre-upgrade fill: V1 events carry no builder attribution, and
                // the SDK reports none rather than guessing
                state::StateEvents::Order(state::OrderEvent {
                    request_id: Some(2),
                    builder: None,
                    r#type: state::OrderEventType::Filled { builder_fee, is_maker, .. },
                    ..
                }) if !*is_maker => {
                    assert_eq!(*builder_fee, udec64!(0));
                    v1_fill_seen = true;
                },

                state::StateEvents::Exchange(state::ExchangeEvent::ContractVersionUpdated(
                    version,
                )) => {
                    assert_eq!(*version, state::ContractVersion::BUILDER_CODES);
                    version_seen = true;
                },
                state::StateEvents::Exchange(state::ExchangeEvent::FeeScheduleUpdated(
                    schedule,
                )) => match schedule.key() {
                    state::FeeScheduleKey::Default => {
                        assert_eq!(schedule.taker_fees(), &TAKER_FEES);
                        assert_eq!(schedule.maker_fees(), &MAKER_FEES);
                        default_schedule_seen = true;
                    },
                    state::FeeScheduleKey::RwaDefault => {
                        // Left blank by the upgrade configuration
                        assert_eq!(schedule.base_taker_fee(), udec64!(0));
                        rwa_schedule_seen = true;
                    },
                    state::FeeScheduleKey::Custom(_) => panic!("no custom schedule was set"),
                },
                // Every contract listed at upgrade time is repointed at the
                // default schedule, and picks up its tiers
                state::StateEvents::Perpetual(state::PerpetualEvent {
                    perpetual_id,
                    r#type: state::PerpetualEventType::FeeScheduleUpdated(schedule),
                }) => {
                    assert_eq!(schedule.key(), state::FeeScheduleKey::Default);
                    assert_eq!(schedule.taker_fees(), &TAKER_FEES);
                    repointed.push(*perpetual_id);
                },

                state::StateEvents::Perpetual(state::PerpetualEvent {
                    perpetual_id,
                    r#type: state::PerpetualEventType::Added,
                }) => listings.push((*perpetual_id, version_seen)),

                // The old generation reports a fee change per contract, with no
                // tiers to it. Retained only for replaying this kind of history.
                state::StateEvents::Perpetual(state::PerpetualEvent {
                    perpetual_id,
                    r#type: state::PerpetualEventType::TakerFeeUpdated(fee),
                }) if *perpetual_id == trx_perp.id => {
                    assert!(!version_seen, "the current contract has no such event");
                    assert_eq!(*fee, udec64!(0.0005));
                    legacy_fee_seen = true;
                },

                // A fill on the contract listed after the upgrade: the base rates
                // are charged, so the fee schedule really was resolved from the
                // key the listing event carried
                state::StateEvents::Order(state::OrderEvent {
                    perpetual_id,
                    request_id: Some(21),
                    r#type: state::OrderEventType::Filled { fee, is_maker, .. },
                    ..
                }) if !*is_maker && *perpetual_id == sol_perp.id => {
                    // 0.5 * 200 * 0.00069
                    assert_eq!(*fee, udec64!(0.069));
                    new_perp_fill_seen = true;
                },
                state::StateEvents::Trade(trade) if trade.taker_request_id == 21 => {
                    // ...and the maker side of the same fill: 0.5 * 200 * 0.00009
                    assert_eq!(trade.maker_fills.first().expect("maker fill").fee, udec64!(0.009));
                },

                // Post-upgrade fill: attribution recovered from the V2 request,
                // fee earned from the V2 fill
                state::StateEvents::Order(state::OrderEvent {
                    request_id: Some(11),
                    builder: None,
                    r#type: state::OrderEventType::Filled { fee, is_maker, .. },
                    ..
                }) if !*is_maker => {
                    // Taker is at the base tier: 0.1 * 100000 * 0.00069
                    assert_eq!(*fee, udec64!(6.9));
                    v2_fill_seen = true;
                },
                state::StateEvents::Trade(trade) if trade.taker_request_id == 11 => {
                    let fill = trade.maker_fills.first().expect("maker fill");
                    assert_eq!(fill.builder.map(|b| b.builder_id()), Some(BUILDER));
                    // 0.1 * 100000 * 0.001
                    assert_eq!(fill.builder_fee, udec64!(10));
                    // ...on top of the maker's base rate: 0.1 * 100000 * 0.00009
                    assert_eq!(fill.fee, udec64!(10.9));
                },

                _ => (),
            }
        }

        if state.request_id_seen(21) {
            break;
        }
    }
    // The raw stream must show both generations' events, so the V1 handling that
    // exists only for this transition is genuinely exercised above rather than
    // inferred from a V2 event that happened to carry no attribution.
    let mut raw = RawEventCounts::default();
    while let Ok(Some(block_events)) =
        tokio::time::timeout(Duration::from_secs(2), state.next_raw_events()).await
    {
        for event in block_events.events() {
            match event.event() {
                ExchangeEvents::OrderRequest(_) => raw.order_request_v1 += 1,
                ExchangeEvents::OrderRequestV2(_) => raw.order_request_v2 += 1,
                ExchangeEvents::MakerOrderFilled(_) => raw.maker_filled_v1 += 1,
                ExchangeEvents::MakerOrderFilledV2(_) => raw.maker_filled_v2 += 1,
                ExchangeEvents::ContractVersionSet(_) => raw.version_set += 1,
                ExchangeEvents::ContractAdded(_) => raw.contract_added_v1 += 1,
                ExchangeEvents::ContractAddedV2(_) => raw.contract_added_v2 += 1,
                _ => (),
            }
        }
        if raw.is_complete() {
            break;
        }
    }
    assert!(raw.order_request_v1 > 0, "no V1 order request in the stream: {raw:?}");
    assert!(raw.maker_filled_v1 > 0, "no V1 maker fill in the stream: {raw:?}");
    assert_eq!(raw.contract_added_v1, 1, "one listing on the old generation: {raw:?}");
    assert_eq!(raw.version_set, 1, "the upgrade stamps the version exactly once: {raw:?}");
    assert!(raw.order_request_v2 > 0, "no V2 order request in the stream: {raw:?}");
    assert!(raw.maker_filled_v2 > 0, "no V2 maker fill in the stream: {raw:?}");
    assert_eq!(raw.contract_added_v2, 1, "one listing after the upgrade: {raw:?}");

    assert!(v1_fill_seen, "pre-upgrade V1 fill not seen");
    assert!(version_seen, "ContractVersionSet not seen");
    assert!(default_schedule_seen, "default fee schedule seed not seen");
    assert!(rwa_schedule_seen, "RWA fee schedule seed not seen");
    assert!(v2_fill_seen, "post-upgrade V2 fill not seen");
    assert!(new_perp_fill_seen, "fill on the contract listed after the upgrade not seen");
    assert!(legacy_fee_seen, "deprecated per-contract fee update not seen");

    // Both listings were tracked, on the correct side of the upgrade
    assert_eq!(
        listings,
        vec![(trx_perp.id, false), (sol_perp.id, true)],
        "one listing on each generation, in order",
    );

    // Every contract listed *at* upgrade time is repointed - including the one
    // listed mid-stream on the old generation, which the upgrade must fold into
    // the keyed fee model. The one listed afterwards is not repointed: it was
    // placed on the default schedule by its own listing.
    repointed.sort();
    repointed.dedup();
    assert_eq!(
        repointed,
        vec![btc_perp.id, eth_perp.id, trx_perp.id],
        "every contract listed at upgrade time must be repointed",
    );
    assert!(!repointed.contains(&sol_perp.id), "a post-upgrade listing needs no repoint");

    // ── state after the upgrade ─────────────────────────────────────────────
    {
        let snapshot = state.snapshot().clone();

        // The version event is authoritative, so the features it implies are now
        // available to a caller that has been streaming since before the upgrade
        assert_eq!(snapshot.contract_version(), Some(state::ContractVersion::BUILDER_CODES));
        assert!(snapshot.features().keyed_fee_schedules());
        assert!(snapshot.features().builder_attribution());
        assert!(snapshot.features().perpetual_discovery());

        // Tiered fees, on both the exchange and every contract - the two learned
        // from the initial snapshot, the one listed on the old generation, and the
        // one listed after the upgrade, whose schedule was resolved from the key
        // its listing event carried and nothing else
        assert_eq!(snapshot.default_fee_schedule().taker_fees(), &TAKER_FEES);
        assert_eq!(snapshot.rwa_fee_schedule().base_taker_fee(), udec64!(0));
        let mut tracked: Vec<_> = snapshot.perpetuals().keys().copied().collect();
        tracked.sort();
        assert_eq!(tracked, vec![btc_perp.id, eth_perp.id, sol_perp.id, trx_perp.id]);
        for perp in snapshot.perpetuals().values() {
            assert_eq!(perp.fee_schedule().key(), state::FeeScheduleKey::Default);
            assert_eq!(perp.fee_schedule().taker_fees(), &TAKER_FEES, "perp {}", perp.id());
            assert_eq!(perp.fee_schedule().maker_fees(), &MAKER_FEES, "perp {}", perp.id());
            assert_eq!(perp.taker_fee_for_tier(2), TAKER_FEES[2]);
        }

        // ...and with real tiers on record, the detailed rendering reports them
        for perp in snapshot.perpetuals().values() {
            assert!(perp.fee_schedule().is_tiered(), "perp {}", perp.id());
            assert!(format!("{perp:#}").contains("Fee tiers (tkr/mkr)"), "perp {}", perp.id());
        }

        // The contract listed after the upgrade is tracked in full, from events
        // alone - it was never part of any snapshot
        let sol = snapshot.perpetuals().get(&sol_perp.id).unwrap();
        assert_eq!(sol.symbol(), "SOL");
        assert!(!sol.is_paused());
        assert_eq!(sol.last_price(), udec64!(200));
        assert_eq!(sol.open_interest(), udec128!(0.5));

        // ...and the same order request the old contract had to reject now
        // prepares an envelope
        let (_, extension) = order(12, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1))
            .with_builder(builder)
            .prepare_v2(&snapshot)
            .expect("upgraded contract carries builder attribution");
        assert_eq!(BuilderAttribution::decode(&extension).unwrap(), Some(builder));
    }
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
