//! End-to-end coverage of posting an order through the SDK alone.
//!
//! Builds the order the way a client does - decimals in human units, checked
//! and scaled against the perpetual - then simulates, sends and waits through
//! [`perpl_sdk::exec::Call`], against an exchange deployed on anvil. No CLI is
//! involved: this is the path any client takes.

use alloy::{
    network::EthereumWallet,
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use fastnum::udec64;
use perpl_sdk::{
    state::{self, SnapshotBuilder},
    testing,
    types::{self, OrderRequest, RequestType},
};

/// A snapshot of the test exchange, tracking `trader`'s positions.
async fn snapshot_of(
    exchange: &testing::TestExchange,
    trader: types::AccountId,
) -> state::Exchange {
    SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
        .with_accounts(vec![types::AccountAddressOrID::ID(trader)])
        .build()
        .await
        .expect("snapshot")
}

/// The signing provider a client builds: its own wallet, stacked on whatever
/// provider it already had.
fn signing_provider(exchange: &testing::TestExchange, pk: &str) -> impl Provider + Clone {
    let signer: PrivateKeySigner = pk.parse().expect("test account key");
    ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_provider(exchange.provider.clone())
}

#[tokio::test]
async fn posts_an_order_that_rests_on_the_book() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;
    let snapshot = snapshot_of(&exchange, trader.id).await;

    // An offer above the mark rests rather than crossing, so the order is
    // still there to be found afterwards
    let request =
        OrderRequest::builder(btc.id, RequestType::OpenShort, udec64!(101000), udec64!(0.5))
            .request_id(4242)
            .build(&snapshot)
            .expect("a valid order");
    assert_eq!(request.request_id(), 4242);

    let receipt = request
        .call(&snapshot, signing_provider(&exchange, &trader.pk), trader.address)
        .expect("a transaction")
        .submit()
        .await
        .expect("the order should be accepted");
    assert!(receipt.status());

    // The decimals the caller typed have to survive the round trip through the
    // perpetual's scaler and back out of the contract unchanged
    let book = snapshot_of(&exchange, trader.id).await;
    let book = book
        .perpetuals()
        .get(&btc.id)
        .expect("btc perpetual")
        .l3_book();
    assert_eq!(book.best_ask(), Some((udec64!(101000), udec64!(0.5))));
    assert_eq!(book.best_bid(), None);
}

#[tokio::test]
async fn simulating_leaves_the_book_untouched() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;
    let snapshot = snapshot_of(&exchange, trader.id).await;

    let call = OrderRequest::builder(btc.id, RequestType::OpenShort, udec64!(101000), udec64!(0.5))
        .build(&snapshot)
        .expect("a valid order")
        .call(&snapshot, signing_provider(&exchange, &trader.pk), trader.address)
        .expect("a transaction");

    // Proving the order would be accepted must not place it, which is what
    // lets a caller show it before asking
    call.simulate().await.expect("the order should not revert");
    assert_eq!(
        snapshot_of(&exchange, trader.id)
            .await
            .perpetuals()
            .get(&btc.id)
            .expect("btc perpetual")
            .total_orders(),
        0,
    );
}

#[tokio::test]
async fn a_simulation_reports_what_the_contract_would_revert_with() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;
    let snapshot = snapshot_of(&exchange, trader.id).await;

    // Far more size than the account's collateral covers at any leverage, so
    // the contract rejects it - and says so before anything is signed
    let err = OrderRequest::builder(btc.id, RequestType::OpenLong, udec64!(100000), udec64!(1000))
        .build(&snapshot)
        .expect("a valid order")
        .call(&snapshot, signing_provider(&exchange, &trader.pk), trader.address)
        .expect("a transaction")
        .simulate()
        .await
        .expect_err("an order beyond the account's collateral");
    assert!(
        matches!(err, perpl_sdk::error::DexError::Provider(_)),
        "expected the contract's own revert, got {}",
        err,
    );
}

#[tokio::test]
async fn rejects_a_price_finer_than_the_perpetual_quotes() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;
    let snapshot = snapshot_of(&exchange, trader.id).await;

    // The BTC test perpetual prices to one decimal place
    let err =
        OrderRequest::builder(btc.id, RequestType::OpenShort, udec64!(101000.123456), udec64!(0.5))
            .build(&snapshot)
            .expect_err("over-precise price")
            .to_string();
    assert!(err.contains("price"), "{}", err);
    assert!(err.contains("101000.1"), "{}", err);
}
