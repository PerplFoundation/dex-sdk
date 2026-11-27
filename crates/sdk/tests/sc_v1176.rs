//! Validates the smart-contract v1.1.7.6 additions across the snapshot and the
//! live event stream:
//!
//! - **Client order IDs**, carried by version 2 of the order-extension
//!   envelope: stored with a placed order and recovered from the account's
//!   order locks on the snapshot path (`getOrderLocksV2`) and from
//!   `OrderRequestV3` on the streaming path; checked by a cancel or change
//!   naming one (`ClientOrderIdMismatch`); replaced or first given by a change;
//!   reported on fills from both sides of a trade; and pre-checked by the
//!   request builder against what the snapshot knows.
//! - **Decrease-collateral requests as order types**: the request, its refusals
//!   (one live request per position, no position) and its cancellation, each
//!   the outcome of the order that carried it.
//! - **Operating maintenance margin fraction**, set and rejected, on both
//!   paths.
//! - **`OrderRequestV3`**: the caller reaches the state events.

use alloy::primitives::Address;
use fastnum::{udec64, udec128};
use futures::FutureExt;
use perpl_sdk::{
    state::{self, SnapshotBuilder},
    stream, testing,
    types::{
        self, ClientOrderId, OrderRequest, OrderRequestBuilderError,
        RequestType::{self, Cancel, Change, OpenLong, OpenShort},
    },
};

const A: u32 = 0xA001;
const B: u32 = 0xA002;
const C: u32 = 0xA003;
const D: u32 = 0xA004;

fn coid(id: u32) -> ClientOrderId { ClientOrderId::new(id).expect("non-zero client order id") }

fn oid(id: u16) -> types::OrderId { types::OrderId::new(id).expect("non-zero order id") }

#[tokio::test]
async fn test_sc_v1176() {
    let exchange = testing::TestExchange::new().await;
    let maker = exchange.account(0, 1_000_000).await;
    let taker = exchange.account(1, 100_000).await;
    // Holds no position, for the request that needs one
    let bystander = exchange.account(2, 10_000).await;
    let btc_perp = exchange.btc_perp().await;

    // An order placed with an ID BEFORE the snapshot, so the ID has to come from
    // the account's order locks rather than from the placement
    _ = btc_perp
        .order_v2(
            maker.id,
            order(1, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1))
                .with_client_order_id(coid(A)),
        )
        .await
        .get_receipt()
        .await
        .unwrap();

    let (indexer, mut state) = testing::Indexer::new(&exchange).await;
    {
        let snapshot = state.snapshot().clone();
        assert_eq!(snapshot.contract_version(), Some(state::ContractVersion::CLIENT_ORDER_IDS));
        let features = snapshot.features();
        assert!(features.client_order_ids());
        assert!(features.decrease_collateral_orders());
        assert!(features.operating_maintenance_margin());

        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        let resting = perp.l3_book().get_order(oid(1)).expect("the resting order");
        assert_eq!(resting.client_order_id(), Some(coid(A)), "recovered from the order locks");
        assert_eq!(resting.placed_request_id(), None, "the placement predates the snapshot");
        assert_eq!(
            perp.l3_book()
                .get_order_by_client_order_id(maker.id, coid(A))
                .map(|order| order.order_id()),
            Some(oid(1)),
        );
        assert_eq!(perp.operating_maintenance_margin(), None, "unset at listing");
        assert_eq!(perp.effective_maintenance_margin(), perp.maintenance_margin());

        // The builder refuses what the exchange would: a cancel naming an ID
        // the order is known not to carry, and a change giving a first ID to an
        // order known to have one
        let refused = OrderRequest::cancel(btc_perp.id, oid(1))
            .client_order_id(coid(B))
            .build(&snapshot);
        assert!(
            matches!(
                refused,
                Err(OrderRequestBuilderError::ClientOrderIdMismatch { expected, stored, .. })
                    if expected == Some(coid(B)) && stored == coid(A)
            ),
            "{refused:?}",
        );
        assert!(matches!(
            OrderRequest::change(btc_perp.id, oid(1))
                .new_client_order_id(coid(B))
                .build(&snapshot),
            Err(OrderRequestBuilderError::ClientOrderIdMismatch { expected: None, .. })
        ));
        // ...while one naming the right ID, or none, builds
        assert!(
            OrderRequest::cancel(btc_perp.id, oid(1))
                .client_order_id(coid(A))
                .build(&snapshot)
                .is_ok()
        );
        assert!(
            OrderRequest::cancel(btc_perp.id, oid(1))
                .build(&snapshot)
                .is_ok()
        );
    }

    tokio::spawn(indexer.run(tokio::time::sleep));

    // ── the client order ID lifecycle ───────────────────────────────────────
    //
    // Refusals revert the single-order entrypoint, so they go through a batch
    // sent without revertOnFail, where the exchange skips the order and emits
    // the refusal instead.

    // 2. A cancel naming the wrong ID is refused and the order survives
    _ = btc_perp
        .orders_v2(
            maker.id,
            vec![
                cancel(2, btc_perp.id, oid(1), udec64!(100000), udec64!(0.1))
                    .with_client_order_id(coid(B)),
            ],
            false,
        )
        .await
        .get_receipt()
        .await
        .unwrap();
    // 3. A change naming the stored ID moves the order and gives it a new one
    _ = btc_perp
        .order_v2(
            maker.id,
            change(3, btc_perp.id, oid(1), udec64!(100100), udec64!(0.1))
                .with_client_order_id(coid(A))
                .with_new_client_order_id(coid(B)),
        )
        .await
        .get_receipt()
        .await
        .unwrap();
    // 4. A cancel sent for the order as it was is refused: it stores B now
    _ = btc_perp
        .orders_v2(
            maker.id,
            vec![
                cancel(4, btc_perp.id, oid(1), udec64!(100100), udec64!(0.1))
                    .with_client_order_id(coid(A)),
            ],
            false,
        )
        .await
        .get_receipt()
        .await
        .unwrap();
    // 5. ...and one naming the new ID takes it off the book
    _ = btc_perp
        .order_v2(
            maker.id,
            cancel(5, btc_perp.id, oid(1), udec64!(100100), udec64!(0.1))
                .with_client_order_id(coid(B)),
        )
        .await
        .get_receipt()
        .await
        .unwrap();
    // 6. The slot is reused by an order placed without an ID...
    _ = btc_perp
        .order_v2(maker.id, order(6, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1)))
        .await
        .get_receipt()
        .await
        .unwrap();
    // 7. ...which a change that alters nothing else gives its first ID
    _ = btc_perp
        .order_v2(
            maker.id,
            change(7, btc_perp.id, oid(1), udec64!(100000), udec64!(0.1))
                .with_new_client_order_id(coid(C)),
        )
        .await
        .get_receipt()
        .await
        .unwrap();
    // 8. A taker naming its own ID fills it in full; the ID of an order that
    // never rests is never stored, but it is still what the taker named
    _ = btc_perp
        .order_v2(
            taker.id,
            order(8, btc_perp.id, OpenLong, udec64!(100000), udec64!(0.1))
                .with_client_order_id(coid(D)),
        )
        .await
        .get_receipt()
        .await
        .unwrap();

    let mut mismatches: Vec<(types::RequestId, Option<ClientOrderId>, Option<ClientOrderId>)> =
        vec![];
    let mut changes: Vec<(types::RequestId, Option<ClientOrderId>)> = vec![];
    let mut removed_by = None;
    let mut placed_without_id = false;
    let mut maker_fill_id = None;
    let mut taker_fill = None;
    while let Some(block_events) = state.next_state_events().await {
        for event in block_events.events().iter().flat_map(|e| e.event()) {
            match event {
                state::StateEvents::Error(state::OrderError {
                    request_id,
                    order_id: Some(order_id),
                    client_order_id,
                    sender,
                    r#type: state::OrderErrorType::ClientOrderIdMismatch { expected, stored },
                    ..
                }) => {
                    assert_eq!(*order_id, oid(1));
                    assert_eq!(*sender, Some(maker.address), "the account sent it itself");
                    assert_eq!(*client_order_id, *expected, "the ID the request named");
                    mismatches.push((*request_id, *expected, *stored));
                },
                state::StateEvents::Order(state::OrderEvent {
                    request_id: Some(request_id),
                    r#type: state::OrderEventType::Updated { client_order_id, .. },
                    ..
                }) if matches!(request_id, 3 | 7) => {
                    changes.push((*request_id, *client_order_id));
                },
                state::StateEvents::Order(state::OrderEvent {
                    request_id,
                    client_order_id,
                    r#type: state::OrderEventType::Removed,
                    ..
                }) if *request_id == Some(5) => {
                    assert_eq!(*client_order_id, Some(coid(B)), "removed as it was");
                    removed_by = *request_id;
                },
                state::StateEvents::Order(state::OrderEvent {
                    request_id: Some(6),
                    placed_request_id,
                    client_order_id,
                    r#type: state::OrderEventType::Placed { .. },
                    ..
                }) => {
                    assert_eq!(*placed_request_id, Some(6));
                    assert_eq!(*client_order_id, None);
                    placed_without_id = true;
                },
                state::StateEvents::Order(state::OrderEvent {
                    request_id: Some(8),
                    client_order_id,
                    sender,
                    r#type: state::OrderEventType::Filled { is_maker, .. },
                    ..
                }) => {
                    if *is_maker {
                        maker_fill_id = Some(*client_order_id);
                    } else {
                        taker_fill = Some((*client_order_id, *sender));
                    }
                },
                _ => (),
            }
        }
        if state.request_id_seen(8) {
            break;
        }
    }
    assert_eq!(
        mismatches,
        vec![(2, Some(coid(B)), Some(coid(A))), (4, Some(coid(A)), Some(coid(B)))],
        "each refusal reports what was named and what is stored",
    );
    assert_eq!(changes, vec![(3, Some(coid(B))), (7, Some(coid(C)))]);
    assert_eq!(removed_by, Some(5));
    assert!(placed_without_id);
    assert_eq!(maker_fill_id, Some(Some(coid(C))), "the maker's stored ID, as the change gave it");
    assert_eq!(taker_fill, Some((Some(coid(D)), Some(taker.address))));
    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        assert_eq!(perp.total_orders(), 0, "filled in full");
    }
    // The trade stream, replaying the same blocks, reports the maker order by
    // the ID the change gave it, which the placement did not carry
    {
        let config = stream::NormalizationConfig::fetch(&exchange.chain(), &exchange.provider)
            .await
            .unwrap();
        let mut processor = stream::TradeProcessor::new(config);
        let mut fills = vec![];
        while let Some(Some(batch)) = state.next_raw_events().now_or_never() {
            fills.extend(
                processor
                    .process_block(&batch)
                    .events()
                    .iter()
                    .flat_map(|e| e.event().maker_fills.clone()),
            );
        }
        let [fill] = fills.as_slice() else { panic!("one maker fill, got {fills:?}") };
        assert_eq!(fill.maker_placed_request_id, Some(6));
        assert_eq!(fill.maker_client_order_id, Some(coid(C)), "as the change gave it");
    }

    // ── decrease-collateral requests as orders ──────────────────────────────
    let snapshot = state.snapshot().clone();
    // 9. The taker, long since request 8, asks for 100 back, clamped
    let request = OrderRequest::request_decrease_collateral(btc_perp.id)
        .amount(udec128!(100))
        .clamp_to_maximum(true)
        .request_id(9)
        .account(taker.id)
        .build(&snapshot)
        .expect("a valid request");
    assert_eq!(request.request_type(), RequestType::RequestDecreasePositionCollateral);
    assert!(request.clamp_to_maximum());
    assert_eq!(request.amount(), Some(udec128!(100)));
    _ = btc_perp
        .order_v2(taker.id, request)
        .await
        .get_receipt()
        .await
        .unwrap();
    // 10. A second one while the first is pending is refused
    let request = OrderRequest::request_decrease_collateral(btc_perp.id)
        .request_id(10)
        .build(&snapshot)
        .expect("a valid request");
    assert_eq!(request.amount(), None, "the maximum");
    _ = btc_perp
        .orders_v2(taker.id, vec![request], false)
        .await
        .get_receipt()
        .await
        .unwrap();
    // 11. ...until it is cancelled
    let request = OrderRequest::cancel_decrease_collateral_request(btc_perp.id)
        .request_id(11)
        .build(&snapshot)
        .expect("a valid request");
    _ = btc_perp
        .order_v2(taker.id, request)
        .await
        .get_receipt()
        .await
        .unwrap();
    // 12. Without a position there is nothing to request against
    let request = OrderRequest::request_decrease_collateral(btc_perp.id)
        .request_id(12)
        .build(&snapshot)
        .expect("a valid request");
    _ = btc_perp
        .orders_v2(bystander.id, vec![request], false)
        .await
        .get_receipt()
        .await
        .unwrap();
    // The builder refuses what the exchange ignores
    assert!(matches!(
        OrderRequest::request_decrease_collateral(btc_perp.id)
            .fill_or_kill(true)
            .build(&snapshot),
        Err(OrderRequestBuilderError::NotApplicable(
            RequestType::RequestDecreasePositionCollateral,
            "fill-or-kill",
        ))
    ));
    assert!(matches!(
        OrderRequest::cancel_decrease_collateral_request(btc_perp.id)
            .amount(udec128!(1))
            .build(&snapshot),
        Err(OrderRequestBuilderError::NotApplicable(
            RequestType::CancelDecreaseCollateralRequest,
            "an amount",
        ))
    ));

    let mut requested = None;
    let mut pending = None;
    let mut cancelled = None;
    let mut without_position = None;
    while let Some(block_events) = state.next_state_events().await {
        for event in block_events.events().iter().flat_map(|e| e.event()) {
            match event {
                state::StateEvents::Position(state::PositionEvent {
                    account_id,
                    request_id: Some(9),
                    r#type:
                        state::PositionEventType::CollateralDecreaseRequested {
                            amount,
                            clamp_to_maximum,
                            expiry_timestamp,
                        },
                    ..
                }) => {
                    assert_eq!(*account_id, taker.id);
                    assert_eq!(*amount, Some(udec128!(100)));
                    assert!(*clamp_to_maximum);
                    requested = Some(*expiry_timestamp);
                },
                state::StateEvents::Error(state::OrderError {
                    request_id: 10,
                    client_order_id: None,
                    r#type:
                        state::OrderErrorType::DecreaseCollateralRequestPending { expiry_timestamp },
                    ..
                }) => pending = Some(*expiry_timestamp),
                state::StateEvents::Position(state::PositionEvent {
                    request_id: Some(11),
                    r#type: state::PositionEventType::CollateralDecreaseRequestCancelled,
                    ..
                }) => cancelled = Some(11),
                state::StateEvents::Error(state::OrderError {
                    account_id,
                    request_id: 12,
                    r#type: state::OrderErrorType::PositionDoesNotExist,
                    ..
                }) => without_position = Some(*account_id),
                _ => (),
            }
        }
        if without_position.is_some() {
            break;
        }
    }
    assert!(requested.is_some_and(|expiry| expiry > 0), "request not reported");
    assert_eq!(pending, requested, "the refusal names the pending request's expiry");
    assert_eq!(cancelled, Some(11));
    assert_eq!(without_position, Some(bystander.id));

    // ── operating maintenance margin fraction ───────────────────────────────
    _ = btc_perp
        .set_operating_maintenance_margin(udec64!(25))
        .await
        .get_receipt()
        .await
        .unwrap();
    // Below the contractual fraction: rejected with an event, not a revert, and
    // nothing changes
    _ = btc_perp
        .set_operating_maintenance_margin(udec64!(15))
        .await
        .get_receipt()
        .await
        .unwrap();
    // The same, through a cancel so the stream has a marker to stop at
    _ = btc_perp
        .order_v2(maker.id, order(13, btc_perp.id, OpenShort, udec64!(100000), udec64!(0.1)))
        .await
        .get_receipt()
        .await
        .unwrap();
    let mut operating_margins = vec![];
    let mut requirements = vec![];
    while let Some(block_events) = state.next_state_events().await {
        for event in block_events.events().iter().flat_map(|e| e.event()) {
            match event {
                state::StateEvents::Perpetual(state::PerpetualEvent {
                    perpetual_id,
                    r#type:
                        state::PerpetualEventType::OperatingMaintenanceMarginFractionUpdated(fraction),
                }) => {
                    assert_eq!(*perpetual_id, btc_perp.id);
                    operating_margins.push(*fraction);
                },
                // Positions are held to the operating fraction from then on:
                // 0.1 * 100000 / 25, down from / 20
                state::StateEvents::Position(state::PositionEvent {
                    account_id,
                    r#type: state::PositionEventType::MaintenanceMarginUpdated(requirement),
                    ..
                }) => requirements.push((*account_id, *requirement)),
                _ => (),
            }
        }
        if state.request_id_seen(13) {
            break;
        }
    }
    assert_eq!(operating_margins, vec![Some(udec64!(25))], "one update, no rejection");
    requirements.sort();
    assert_eq!(
        requirements,
        vec![(maker.id, udec128!(400)), (taker.id, udec128!(400))],
        "both positions re-held once, not again on the rejection",
    );
    {
        let snapshot = state.snapshot().clone();
        let perp = snapshot.perpetuals().get(&btc_perp.id).unwrap();
        assert_eq!(perp.operating_maintenance_margin(), Some(udec64!(25)));
        assert_eq!(perp.effective_maintenance_margin(), udec64!(25));
        assert!(format!("{perp:#}").contains("ini/mnt/opr"));
        let position = &snapshot.accounts()[&taker.id].positions()[&btc_perp.id];
        assert_eq!(position.maintenance_margin_requirement(), udec128!(400));
        // Long 0.1 @ 100000 with 1000 deposited: liquidated where the value
        // falls to the requirement, (400 - 1000) / 0.1 below entry
        assert_eq!(position.liquidation_price(), udec64!(94000));
    }

    // ...and a fresh snapshot reads it through the getter
    let fresh = SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
        .with_accounts(vec![types::AccountAddressOrID::ID(maker.id)])
        .build()
        .await
        .unwrap();
    let perp = fresh.perpetuals().get(&btc_perp.id).unwrap();
    assert_eq!(perp.operating_maintenance_margin(), Some(udec64!(25)));
    // ...and holds the positions it loads to it
    assert_eq!(
        fresh.accounts()[&maker.id].positions()[&btc_perp.id].maintenance_margin_requirement(),
        udec128!(400),
    );
    // The order placed without an ID stores none, which the locks report as
    // none too
    assert_eq!(perp.l3_book().get_order(oid(1)).unwrap().client_order_id(), None);
    assert_ne!(maker.address, Address::ZERO);
}

/// An order request with the boilerplate parameters filled in.
fn order(
    request_id: types::RequestId,
    perp_id: types::PerpetualId,
    r#type: RequestType,
    price: fastnum::UD64,
    size: fastnum::UD64,
) -> OrderRequest {
    OrderRequest::new(
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

/// A cancel of `order_id`, which the contract wants restated with its price
/// and size.
fn cancel(
    request_id: types::RequestId,
    perp_id: types::PerpetualId,
    order_id: types::OrderId,
    price: fastnum::UD64,
    size: fastnum::UD64,
) -> OrderRequest {
    OrderRequest::new(
        request_id,
        perp_id,
        Cancel,
        Some(order_id),
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

/// A change of `order_id` to `price` and `size`.
fn change(
    request_id: types::RequestId,
    perp_id: types::PerpetualId,
    order_id: types::OrderId,
    price: fastnum::UD64,
    size: fastnum::UD64,
) -> OrderRequest {
    OrderRequest::new(
        request_id,
        perp_id,
        Change,
        Some(order_id),
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
