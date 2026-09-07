//! End-to-end coverage of the `order` commands against a deployed exchange.
//!
//! These drive [`perpl_cli::run`] through the same argument parsing a user
//! goes through, so they exercise the wiring - arguments to the SDK's builder,
//! to a signed submission - rather than the order logic itself, which is the
//! SDK's own `order_submit` suite.

use clap::Parser;
use fastnum::udec64;
use perpl_cli::args::Cli;
use perpl_sdk::{state::SnapshotBuilder, testing, types};

/// Builds the argument vector a user would type, pointed at the test exchange.
/// The signing key is left to `extra` so each test can choose its source.
fn cli(rpc: &str, exchange: &str, perp: &str, extra: &[&str]) -> Cli {
    let mut argv = vec![
        "perpl-cli",
        "--rpc",
        rpc,
        "--exchange",
        exchange,
        "--perp",
        perp,
        "order",
        "create",
        "--yes",
    ];
    argv.extend_from_slice(extra);
    Cli::try_parse_from(argv).expect("valid arguments")
}

#[tokio::test]
async fn posts_an_order_that_rests_on_the_book() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;

    let rpc = exchange.rpc_url.clone();
    let address = exchange.exchange.address().to_string();
    let perp = btc.id.to_string();

    // An offer above the mark rests rather than crossing, so the order is
    // still there to be found afterwards
    perpl_cli::run(cli(
        &rpc,
        &address,
        &perp,
        &[
            "--private-key",
            &trader.pk,
            "--side",
            "sell",
            "--size",
            "0.5",
            "--price",
            "101000",
            "--request-id",
            "4242",
        ],
    ))
    .await
    .expect("order create should succeed");

    let snapshot = SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
        .with_accounts(vec![types::AccountAddressOrID::ID(trader.id)])
        .build()
        .await
        .expect("snapshot");
    let book = snapshot
        .perpetuals()
        .get(&btc.id)
        .expect("btc perpetual")
        .l3_book();

    // The decimals the user typed have to survive the round trip through the
    // perpetual's scaler and back out of the contract unchanged
    assert_eq!(book.best_ask(), Some((udec64!(101000), udec64!(0.5))));
    assert_eq!(book.best_bid(), None);
}

#[tokio::test]
async fn a_dry_run_leaves_the_book_untouched() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;

    perpl_cli::run(cli(
        &exchange.rpc_url,
        &exchange.exchange.address().to_string(),
        &btc.id.to_string(),
        &[
            "--private-key",
            &trader.pk,
            "--side",
            "sell",
            "--size",
            "0.5",
            "--price",
            "101000",
            "--dry-run",
        ],
    ))
    .await
    .expect("dry run should succeed");

    let snapshot = SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
        .with_accounts(vec![types::AccountAddressOrID::ID(trader.id)])
        .build()
        .await
        .expect("snapshot");
    // Simulated, never sent: the whole point of the flag, and the one step
    // that is the CLI's alone to get wrong
    assert_eq!(
        snapshot
            .perpetuals()
            .get(&btc.id)
            .expect("btc perpetual")
            .total_orders(),
        0,
    );
}

#[tokio::test]
async fn rejects_a_price_finer_than_the_perpetual_quotes() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;

    // The BTC test perpetual prices to one decimal place. The SDK faults on
    // the `price` field; naming the flag the user typed is the CLI's job
    let err = perpl_cli::run(cli(
        &exchange.rpc_url,
        &exchange.exchange.address().to_string(),
        &btc.id.to_string(),
        &[
            "--private-key",
            &trader.pk,
            "--side",
            "sell",
            "--size",
            "0.5",
            "--price",
            "101000.123456",
        ],
    ))
    .await
    .expect_err("over-precise price should be rejected")
    .to_string();
    assert!(err.contains("--price"), "{}", err);
    assert!(err.contains("101000.1"), "{}", err);
}

/// Posts one resting ask through the CLI and returns its exchange-assigned ID.
async fn resting_ask(
    exchange: &testing::TestExchange,
    trader: &testing::TestAccount<'_>,
    btc: types::PerpetualId,
) -> types::OrderId {
    perpl_cli::run(cli(
        &exchange.rpc_url,
        &exchange.exchange.address().to_string(),
        &btc.to_string(),
        &[
            "--private-key",
            &trader.pk,
            "--side",
            "sell",
            "--size",
            "0.5",
            "--price",
            "101000",
            // The exchange takes the request ID as an idempotency key and wants
            // it strictly increasing, so the two commands are numbered rather
            // than left to the clock
            "--request-id",
            "1",
        ],
    ))
    .await
    .expect("order create should succeed");

    book(exchange, trader.id, btc)
        .await
        .ask_orders()
        .next()
        .expect("the order should be resting")
        .order_id()
}

/// The perpetual's book as a fresh snapshot sees it.
async fn book(
    exchange: &testing::TestExchange,
    trader: types::AccountId,
    btc: types::PerpetualId,
) -> perpl_sdk::state::OrderBook {
    SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
        .with_accounts(vec![types::AccountAddressOrID::ID(trader)])
        .build()
        .await
        .expect("snapshot")
        .perpetuals()
        .get(&btc)
        .expect("btc perpetual")
        .l3_book()
        .clone()
}

#[tokio::test]
async fn delete_takes_a_resting_order_off_the_book() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;
    let order_id = resting_ask(&exchange, &trader, btc.id).await;

    // `delete` is the alias; the exchange calls it a cancel
    perpl_cli::run(
        Cli::try_parse_from([
            "perpl-cli",
            "--rpc",
            &exchange.rpc_url,
            "--exchange",
            &exchange.exchange.address().to_string(),
            "--perp",
            &btc.id.to_string(),
            "order",
            "delete",
            "--yes",
            "--private-key",
            &trader.pk,
            "--order-id",
            &order_id.to_string(),
            "--request-id",
            "2",
        ])
        .expect("valid arguments"),
    )
    .await
    .expect("order delete should succeed");

    assert_eq!(book(&exchange, trader.id, btc.id).await.total_orders(), 0);
}

#[tokio::test]
async fn update_moves_a_resting_order_and_leaves_its_size_alone() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;
    let order_id = resting_ask(&exchange, &trader, btc.id).await;

    perpl_cli::run(
        Cli::try_parse_from([
            "perpl-cli",
            "--rpc",
            &exchange.rpc_url,
            "--exchange",
            &exchange.exchange.address().to_string(),
            "--perp",
            &btc.id.to_string(),
            "order",
            "update",
            "--yes",
            "--private-key",
            &trader.pk,
            "--order-id",
            &order_id.to_string(),
            // Only the price: the size is the resting order's, which the SDK
            // reads off the book rather than making the caller restate it
            "--price",
            "102000",
            "--request-id",
            "2",
        ])
        .expect("valid arguments"),
    )
    .await
    .expect("order update should succeed");

    assert_eq!(
        book(&exchange, trader.id, btc.id).await.best_ask(),
        Some((udec64!(102000), udec64!(0.5))),
    );
}

#[tokio::test]
async fn an_update_that_amends_nothing_says_which_flags_to_pass() {
    let exchange = testing::TestExchange::new().await;
    let trader = exchange.account(0, 1_000_000).await;
    let btc = exchange.btc_perp().await;
    let order_id = resting_ask(&exchange, &trader, btc.id).await;

    let err = perpl_cli::run(
        Cli::try_parse_from([
            "perpl-cli",
            "--rpc",
            &exchange.rpc_url,
            "--exchange",
            &exchange.exchange.address().to_string(),
            "--perp",
            &btc.id.to_string(),
            "order",
            "update",
            "--yes",
            "--private-key",
            &trader.pk,
            "--order-id",
            &order_id.to_string(),
        ])
        .expect("valid arguments"),
    )
    .await
    .expect_err("an amendment of nothing should be rejected")
    .to_string();
    // The SDK faults on the request; naming the flags that would fix it is the
    // CLI's job
    assert!(err.contains("--price"), "{}", err);
    assert!(err.contains("--expiry-block"), "{}", err);
}
