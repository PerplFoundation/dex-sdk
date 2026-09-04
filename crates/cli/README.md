# perpl-cli

Command line tool to read Perpl exchange state and events, and to place orders.

Every command but `order` is read-only and needs no keys or configuration.
`order create` signs and submits a transaction, so it needs a private key. By
default the tool talks to Monad mainnet over a public RPC endpoint.

## Install

```bash
cargo install perpl-cli
```

## Usage

```
perpl-cli [OPTIONS] <COMMAND>
```

Show the ten most recent trades on mainnet:

```bash
perpl-cli show trades
```

### Commands

- `block <BLOCK_NUMBER>`: Trace raw events from a particular block
- `order`: Place an order on a perpetual contract
    - `create`: Post a single order to the perpetual given by `--perp`
- `show`: Show live state of account, perpetual order book or recent trades
    - `account`: Show account state
        - `--num-trades <N>`: Number of most recent trades to show, 0 to omit
          them [default: 10]
    - `book`: Show state of perpetual order book
        - `-d`, `--depth <N>`: Number of price levels to display, 0 for all
          [default: 10]
        - `--orders-per-level <N>`: Maximum orders to show per level, 0 for all
          [default: 10]
        - `--show-expired`: Also show expired orders
    - `mms <ACCOUNT[:LABEL]>...`: Show how the given market makers are
      distributed across a perpetual order book. Takes the same `--depth`,
      `--orders-per-level` and `--show-expired` options as `book`.
    - `trades`: Show recent trades
- `snapshot`: Take a snapshot of exchange state at a particular block height
- `trace`: Take an initial snapshot, then trace all events, then print the final state
- `tx <TX_HASH>`: Trace raw events from a particular transaction

### Options

These apply to every command.

- `--rpc <RPC>`: RPC endpoint to connect to [default: <https://rpc.monad.xyz> for
  mainnet, <https://testnet-rpc.monad.xyz> for testnet]
- `--testnet`: Use testnet provider and contract addresses [default: mainnet]
- `--rpc-throttle <REQ_PER_SEC>`: RPC throttling (req/sec) [default: 15 for the
  default RPC providers, none for a custom `--rpc`]
- `--exchange <ADDRESS>`: Exchange smart contract address [default: the mainnet or
  testnet deployment]
- `--block <BLOCK>`: Block number to fetch state at or start tracing from [default:
  latest block]
- `--num-blocks <NUM_BLOCKS>`: Number of blocks to trace or show [default: unlimited,
  until terminated by Ctrl+C]
- `--account <ADDRESS or ACCOUNT_ID>`: Account addresses or ID to snapshot/trace/show
  [default: all accounts for `snapshot`/`trace`, required for `show account`]
- `--perp <PERPETUAL_ID>`: Perpetual ID to show state/trace for [default: all
  perpetuals for `snapshot`/`trace`/`show trades`, required for `show book` and
  `show mms`]
- `--highlight <ADDRESS or ACCOUNT_ID>`: Paint everything one account is behind
  on a contrasting background [default: no highlighting]

## Placing an order

`order create` posts one order to the perpetual named by `--perp`. It is the
only command that signs a transaction.

```bash
# Bid 0.001 BTC at 65432.1 on mainnet BTC, resting on the book
perpl-cli --perp 1 order create --private-key-path ~/.perpl/key \
  --side buy --size 0.001 --price 65432.1
```

Price, size and leverage are given in human units - `65432.1`, not the
fixed-point integer the contract stores. Each perpetual carries its own price
and lot precision, and the value is scaled by that. A value finer than the
perpetual accepts is rejected rather than rounded, naming what it would have
become:

```
Error: --price 65432.123456 carries more precision than perpetual's 1 decimal
place(s) allows; it would become 65432.1
```

Before anything is signed the command prints the order it built, then simulates
the call, then asks to confirm. `--dry-run` stops after the simulation and
prints the calldata; `--yes` skips the prompt, which a non-interactive run must
pass explicitly.

### Options

- `--side <buy|sell>`: Side of the book to post on. With `--reduce-only`, `sell`
  becomes a close-long and `buy` a close-short
- `--size <DECIMAL>` (alias `--amount`): Order size, in the perpetual's lot
  precision
- `--price <DECIMAL>`: Limit price, in the perpetual's price precision. Required
  even with `--ioc`, where it bounds how far the fill may run
- `--leverage <DECIMAL>`: Leverage to open at [default: the perpetual's maximum]
- `--reduce-only`: Only reduce an existing position
- `--post-only` / `--ioc` / `--fok`: Reject rather than take liquidity / cancel
  what does not fill immediately / fill in full or not at all
- `--expiry-block <BLOCK>`: Block the order expires at [default: never]
- `--max-matches <N>`: Maximum resting orders to match against [default:
  unlimited]
- `--max-neg-pnl-collat-bps <BPS>`: Additional collateral, in basis points of
  notional, the exchange may draw to cover the position's negative unrealized
  PnL on a fill [default: 0]
- `--request-id <ID>`: Client order ID to tag the order with [default: derived
  from the current time]
- `--builder-id <ID>` / `--builder-fee <DECIMAL>`: Attribute the order to a
  builder at that fee rate. Both are required together, and the deployed
  contract has to support builder attribution
- `--private-key-path <PATH>`: File to read the signing key from, whitespace
  trimmed. Takes precedence over the other two sources
- `--private-key <KEY>`: Key to sign with, or `PERPL_PRIVATE_KEY`
- `--gas-limit <GAS>`: Gas limit [default: estimated]
- `--dry-run`: Build and simulate, print what would be sent, then stop
- `-y`, `--yes`, `--auto-confirm`: Submit without the confirmation prompt

### The signing key

Three sources, in precedence order: `--private-key-path`, then `--private-key`,
then `PERPL_PRIVATE_KEY`. The file wins outright rather than conflicting with
the others, because `PERPL_PRIVATE_KEY` is the sort of thing a shell profile
exports once - refusing to run whenever it happens to be set would make
`--private-key-path` unusable in the setup that most wants it.

```bash
# Best: the key never appears in argv or the environment
perpl-cli --perp 1 order create --private-key-path ~/.perpl/key \
  --side buy --size 0.001 --price 65432.1
```

Prefer a file or the environment variable over `--private-key`: an argument is
visible to every other process on the machine through `ps`, and lands in shell
history.

The key is never rendered. It is held in a wrapper whose `Debug` and `Display`
both print `[redacted]`, so it cannot reach the terminal through a debug print
of the parsed arguments, a panic, an error chain, or `--help` with
`PERPL_PRIVATE_KEY` set. A key that fails to parse is reported without echoing
what was read.

`--yes` has no environment variable by design: one exported in a shell profile
would silently arm every later order.

The account must already exist: the exchange opens one on deposit, so deposit
collateral before placing a first order.

## Following one account

`--highlight` picks an account out of the output wherever it appears: the raw
and state events of `trace`, `block` and `tx`, the resting orders of `show book`
and `show mms`, and both sides of every fill in `show trades`.

```bash
# Watch one market maker work the BTC book
perpl-cli --perp 1 --highlight 4638 show book

# ... and see exactly which events in a block were theirs
perpl-cli --highlight 0x1234...abcd block 101375850
```

## Market maker analysis

`show mms` takes the accounts to track as `ACCOUNT[:LABEL]` pairs - an address
or an account ID, optionally labelled - repeated or comma-separated:

```bash
perpl-cli --perp 1 show mms 4638:Alpha 0x1234...abcd:Beta,5022
```

Each maker gets its own background colour, keyed by a legend, and its orders are
painted in it throughout the book below. Above the book sit two tables:

- **Resting quotes**, from the book as it stands: orders and price levels per
  side, size and its share of that side of the book, notional, the maker's own
  best bid and ask, the spread between them in basis points, how far from the
  mid its furthest quote rests, the size it keeps within 10 and 50 bps of the
  mid with its share of all the book's depth in that band, and the imbalance
  between its two sides. Closed by an `Others` row for
  the untracked remainder and a `Book` row for the whole book, so every share
  can be read against its total.
- **Activity** on that perpetual, accumulated from the event stream since the
  command started: maker and taker fills, each with their size, notional and
  fees, and the quotes placed, amended and cancelled - fills excluded, so the
  counts measure quote churn rather than trading.
