# perpl-cli

Command line tool to read Perpl exchange state and events.

Read-only: it currently doesn't sign or send transactions, and needs no keys or
configuration. By default it talks to Monad mainnet over a public RPC endpoint.

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
