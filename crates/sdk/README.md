# perpl-sdk

Rust SDK for the [Perpl](https://perpl.xyz) decentralized perpetuals exchange on
[Monad](https://monad.xyz).

It maintains an in-memory cache of on-chain exchange state — perpetual
contracts, L3 order books, accounts and positions — kept up to date from the
contract's event stream, and builds, simulates and submits orders to the
exchange.

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
[`stream::EventSource`] (or the simpler, polling-only [`stream::raw`]) into
`Exchange::apply_events` to catch up and stay current.

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

    // Keep the snapshot up to date, starting from the block right after it:
    // one batch per block once it is voted on (the `safe` tag), the same
    // cadence `stream::raw` has, but catching up in block ranges.
    let (events, _handle) =
        stream::EventSource::new(&chain, provider, exchange.instant().next()).build(tokio::time::sleep);
    let mut events = pin!(stream::once(events));
    while let Some(block_events) = events.next().await {
        let state_events = exchange.apply_events(&block_events?)?;
        for event in state_events.iter().flat_map(|block| block.events()) {
            println!("{event:?}");
        }
    }
    Ok(())
}
```

### Commit states, websockets and speculative state

Monad executes a block as soon as it is proposed and announces it again at
every later consensus stage. `EventSource` follows that: it delivers every
block at the lowest `types::CommitState` asked for (`Proposed`, `Voted` or
`Finalized`) and once more when it finalizes, tagging each batch with its
state, block hash and consensus block id. Subscribing over websocket (feature
`ws`) gets a `Proposed` block about 300 ms before `Voted` and 600 ms before
`Finalized`; polling fills every gap, so the stream is continuous and
delivered exactly once per block and state no matter what the subscription
does. A proposal consensus abandons shows up as a later batch at the same
height with `supersedes()` set, which invalidates every speculative batch
from that height up.

`Exchange::apply_events_at` takes the commit state to *trust*: blocks at or
above it update the committed snapshot, blocks below it update a speculative
overlay (`Exchange::speculative()`) and still produce state events, so a
consumer can react to a proposed fill immediately and settle on the finalized
one:

```rust
use perpl_sdk::{state::SnapshotBuilder, stream, types::CommitState};

let mut exchange = SnapshotBuilder::new(&chain, provider.clone())
    .with_accounts(vec![account])
    .at_commit_state(CommitState::Finalized) // never hold a revertible block
    .build()
    .await?;

let (events, handle) = stream::EventSource::new(&chain, provider, exchange.instant().next())
    .min_commit_state(CommitState::Proposed)
    .with_ws_url("wss://rpc.monad.xyz") // feature `ws`; repeatable, earliest wins
    .build(tokio::time::sleep);
let mut events = pin!(events);
while let Some(block) = events.next().await {
    let block = block?;
    // Proposed and Voted batches go to the overlay, Finalized ones commit
    if let Some(state_events) = exchange.apply_events_at(&block, CommitState::Finalized)? {
        println!(
            "{} [{}{}] {} state events; speculative book at {}",
            block.instant(),
            block.commit_state(),
            if block.supersedes() { ", supersedes" } else { "" },
            state_events.events().len(),
            exchange.latest_instant(),
        );
    }
}
let _ = handle.status(); // connection and cursor state, for monitoring
```

The `stream::once` adapter turns the multi-level stream back into one batch
per block (the first at or above the minimum), and `stream::finalized` keeps
only finalized ones. Both `stream::raw` and `Exchange::apply_events` keep
working as before; they are the `Voted` and the commit-everything special
cases of the above.

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

[`stream::trade`] wraps a raw stream into normalized trade events, batching
all maker fills per taker and converting fixed-point values to decimals. Fed
from an `EventSource` with `min_commit_state(Proposed)` it yields the trades
of a proposed block right away and again, tagged `Finalized`, when the block
finalizes:

```rust
let (raw, _) = stream::EventSource::new(&chain, provider.clone(), exchange.instant().next())
    .build(tokio::time::sleep);
let trades = stream::trade(&chain, provider, raw).await?;
```

### Posting orders

`types::OrderRequest::builder` builds an order from decimals in human units -
`65432.1`, not the fixed-point integer the contract stores. Its `build` step
quantizes them against the perpetual's own price, lot and leverage precision,
and rejects what the exchange would: a halted exchange, a paused perpetual,
leverage above the cap, contradictory flags, builder attribution a deployed
contract cannot carry, and a value finer than the perpetual accepts - which the
contract itself would silently truncate.

`exec::Call` then takes it to the chain in four steps, so a client can print,
confirm or abandon the order between any two of them:

```rust
use alloy::{network::EthereumWallet, providers::ProviderBuilder};
use perpl_sdk::types::{OrderRequest, RequestType};

// Bid 0.001 BTC at 65432.1, resting on the book, at the perpetual's maximum
// leverage
let request = OrderRequest::builder(perp_id, RequestType::OpenLong, price, size)
    .post_only(true)
    .build(&exchange)?;

// Signing is the caller's: stack a wallet on the provider already in hand,
// which keeps its throttling, retry and poll settings
let signer = ProviderBuilder::new()
    .wallet(EthereumWallet::from(wallet))
    .connect_provider(provider);

let call = request.call(&exchange, signer, from)?;
call.simulate().await?;                    // would it revert?
let sent = call.send().await?;             // signed and on the wire
println!("submitted {}", sent.tx_hash());
let receipt = sent.wait().await?;          // errors on a reverted status
```

`Call::submit` is those last three steps in one, for a client that needs
nothing in between. A batch goes through `exec::orders_call`, and every
operation the exchange takes on an order goes through the same path.

### Cancelling and changing orders

`OrderRequest::cancel` and `OrderRequest::change` name an order that is already
on the book, so they read what they need off the snapshot rather than making
the caller restate it:

```rust
// Nothing but the ID: the price and size the contract wants come from the
// snapshot's own book entry for the order
OrderRequest::cancel(perp_id, order_id).build(&exchange)?;

// Move an order to another level. Its size, expiry, leverage and builder
// attribution are the resting order's, untouched
OrderRequest::change(perp_id, order_id).price(new_price).build(&exchange)?;

// Or resize it in place, leaving the level alone. Sizing down keeps the
// order's queue priority; sizing up sends it to the back
OrderRequest::change(perp_id, order_id).size(new_size).build(&exchange)?;
```

A change is one operation where a cancel and a re-post is two, at roughly half
the gas. `build` rejects what the exchange would: an order that is not on the
book (`DexError::OrderNotFound`), a change of a close order, a change of an
expired order that does not set a new expiry, and a change that amends nothing
at all. A cancel is deliberately *not* blocked by a halted exchange or a paused
perpetual - getting out is not the same as getting in.

Both need a snapshot that tracks the perpetual's book, which
`SnapshotBuilder` does by default.

For lower-level control, `prepare_v2` still returns the order descriptor and
its extension envelope for a hand-built
`abi::dex::Exchange::ExchangeInstance::execOrdersV2` call, and `prepare`
targets the V1 entrypoints, which cannot carry builder attribution.

## Features

| Feature | Default | Description |
| --- | --- | --- |
| `display` | yes | `std::fmt::Display` implementations for state types (order book views, account and exchange rendering). |
| `testing` | yes | The `testing` module: a local Anvil-based exchange deployment. Pulls in `alloy/node-bindings`. |
| `test-utils` | no | Test builders (`Perpetual::for_test`, `with_bid`, `with_ask`, …) for downstream crates, without exposing internal mutation methods in production builds. |
| `ws` | no | WebSocket subscriptions for `stream::EventSource::with_ws_url`. Pulls in alloy's pubsub transport, which needs a tokio runtime. |

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
  exchange state and events, and for placing, cancelling and changing orders.

## License

MIT
