# perpl-sdk

Rust SDK for the [Perpl](https://perpl.xyz) decentralized perpetuals exchange on
[Monad](https://monad.xyz).

It maintains an in-memory cache of on-chain exchange state — perpetual
contracts, L3 order books, accounts and positions — kept up to date from the
contract's event stream, and provides helpers for building order requests to
send to the exchange.

## Install

```bash
cargo add perpl-sdk
```

Requires Rust >= 1.85.0 (edition 2024).

Read-only consumers that do not need the local test environment can drop the
default features, which avoids pulling in the Anvil node bindings:

```toml
perpl-sdk = { version = "0.2", default-features = false, features = ["display"] }
```

## Usage

State tracking is two steps: take a consistent snapshot at some block with
[`state::SnapshotBuilder`], then feed the raw event stream from
[`stream::raw`] into `Exchange::apply_events` to catch up and stay current.

```rust
use std::pin::pin;

use alloy::{primitives::address, providers::ProviderBuilder};
use futures::StreamExt;
use perpl_sdk::{Chain, state::SnapshotBuilder, stream, types};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let chain = Chain::mainnet();
    let provider = ProviderBuilder::new().connect("https://rpc.monad.xyz").await?;

    // Snapshot one account, across every perpetual listed on the exchange,
    // at the latest safe block. The account has to exist already.
    let account = types::AccountAddressOrID::Address(
        address!("0x1111111111111111111111111111111111111111"), // your account
    );
    let mut exchange = SnapshotBuilder::new(&chain, provider.clone())
        .with_accounts(vec![account])
        .build()
        .await?;
    println!("{} perpetuals @ {}", exchange.perpetuals().len(), exchange.instant());

    // Keep the snapshot up to date, starting from the block right after it.
    let events = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut events = pin!(events);
    while let Some(block_events) = events.next().await {
        let state_events = exchange.apply_events(&block_events?)?;
        for event in state_events.iter().flat_map(|block| block.events()) {
            println!("{event:?}");
        }
    }
    Ok(())
}
```

`Chain::mainnet()` and `Chain::testnet()` carry the deployed contract addresses;
`Chain::custom` points the SDK at another deployment. By default the SDK
discovers the listed perpetuals on-chain — configure `with_perpetuals` only to
track a deliberate subset. Accounts, in contrast, are tracked only when named:
pass them to `with_accounts` (by address or by exchange account ID), or use
`with_all_positions` to pick up every open position instead, without per-account
state.

The deployed contract may lag behind the revision the SDK targets, so the
snapshot detects its `state::ContractFeatures` and degrades gracefully rather
than failing on a missing selector.

### Trades

[`stream::trade`] wraps the raw stream into normalized trade events, batching
all maker fills per taker and converting fixed-point values to decimals:

```rust
let raw = stream::raw(&chain, provider.clone(), exchange.instant().next(), tokio::time::sleep);
let trades = stream::trade(&chain, provider, raw).await?;
```

### Posting orders

Build a `types::OrderRequest`, then `prepare_v2` it against the snapshot to get
an order descriptor and its extension envelope for
`abi::dex::Exchange::ExchangeInstance::execOrdersV2`. The snapshot supplies the
per-perpetual decimal conversions, so requests are expressed in decimal prices,
sizes and leverage rather than raw fixed-point values. `prepare` targets the V1
entrypoints, which cannot carry builder attribution.

Note that the SDK currently doesn't sign or send transactions itself — it
prepares the call data and leaves signing to your [alloy](https://alloy.rs)
provider.

## Features

| Feature | Default | Description |
| --- | --- | --- |
| `display` | yes | `std::fmt::Display` implementations for state types (order book views, account and exchange rendering). |
| `testing` | yes | The `testing` module: a local Anvil-based exchange deployment. Pulls in `alloy/node-bindings`. |
| `test-utils` | no | Test builders (`Perpetual::for_test`, `with_bid`, `with_ask`, …) for downstream crates, without exposing internal mutation methods in production builds. |

## Testing

The `testing` module spins up an Anvil instance with the collateral token and
exchange contracts deployed, and provides helpers for configuring perpetual
contracts, creating accounts, posting orders and synchronizing an indexer in
tests. It needs the `anvil` binary from the
[Monad Foundry fork](https://github.com/category-labs/foundry/releases/tag/v1.5.0-monad.0.2.0)
on `PATH`.

```rust
let exchange = perpl_sdk::testing::TestExchange::new().await;
let maker = exchange.account(0, 1_000_000).await;
let btc_perp = exchange.btc_perp().await;
```

See the crate's `tests/` directory for worked examples.

## Documentation

```bash
cargo doc -p perpl-sdk --no-deps --open
```

More usage examples live in
[PerplFoundation/dex-sdk-examples](https://github.com/PerplFoundation/dex-sdk-examples).

## Related crates

- [perpl-cli](https://crates.io/crates/perpl-cli): CLI for reading and tracing
  exchange state and events.

## License

MIT
