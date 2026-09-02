# perpl-cli

Command line tool to read Perpl exchange state and events.

Read-only: it never signs or sends transactions, and needs no keys or
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
  perpetuals for `snapshot`/`trace`/`show trades`, required for `show book`]
