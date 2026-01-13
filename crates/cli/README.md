# perpl-cli

Command line tool to read Perpl exchange state and events.

## Usage

```
perpl-cli [OPTIONS] COMMAND
``` 

### Commands

- `snapshot`: Take a snapshot of exchange state at a particular block height
- `trace`: Take an initial snapshot, then trace all events, then print the final state
- `show`: Show live state of account, perpetual order book or recent trades
    - `account`: Show account state
    - `book`: Show state of perpetual order book
    - `trades`: Show recent trades

### Options

- `--rpc <RPC>`: RPC endpoint to connect to [default: https://testnet-rpc.monad.xyz]
- `--rpc_throttle <REQ_PER_SEC>`: RPC throttling (req/sec) [default: 15 for default RPC provider and none for custom]
- `--exchange <ADDRESS>`: Exchange smart contract address [default: [`Chain::testnet().exchange()`]]
- `--block <BLOCK>`: Block number to fetch state at or start tracing from [default: latest block]
- `--num-blocks <NUM_BLOCKS>`: Number of blocks to trace or show [default: unlimited, until terminated by (Ctrl+C)]
- `--account <ADDRESS or ACCOUNT_ID>`: Account addresses or ID to snaphot/trace/show [default: all accounts for `snapshot`/`trace`, required for `show account`]
- `--perp <PERPETUAL_ID>`: Perpetual ID to show state/trace for [default: all perpetuals for `snapshot`/`trace`/`show trades`, required for `show book`]
