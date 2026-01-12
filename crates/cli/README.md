# perpl-cli

Command line tool to read Perpl exchange state and events.

## Usage

```
perpl-cli [OPTIONS] COMMAND
``` 

### Commands

- `snapshot`: Take a single snapshot of the exchange state at a particular block and render it according to the provided display options.
- `trace`: Take and render an initial snapshot, then render smart contract events and derived SDK events per transaction, then render the final state.
- `show`: Render live state of particular account, perpetual order book or trades.
    - `account`: Render live state of particular account.
    - `book`: Render live state of particular perpetual order book.
    - `trades`: Render live trades.

### Options

- `--rpc <RPC>`: RPC endpoint to connect to (default: https://testnet-rpc.monad.xyz)
- `--rpc_throttle <REQ_PER_SEC>`: RPC endpoint throttling, defaults to 15 for default RPC provider and none for custom
- `--exchange <ADDRESS>`: Exchange smart contract address (default: [`Chain::testnet().exchange()`])
- `--block <BLOCK>`: Block number to fetch state at or start tracing from (default: latest)
- `--num-blocks <NUM_BLOCKS>`: Number of blocks to trace (default: until terminated)
- `--accounts <ADDRESS or ACCOUNT_ID>`: Addresses or IDs of the accounts to show state/trace for (default: all accounts for `snapshot`/`trace`, required for `show account`)
- `--account <ADDRESS or ACCOUNT_ID>`: Address or ID of the account to show, required for `show account`
- `--perps <PERPETUAL_ID>`: Perpetual IDs to show state/trace for (default: all perpetuals for `snapshot`/`trace`/`show trades`
- `--perp <PERPETUAL_ID>`: Perpetual ID to show order book of, required for `show book`
