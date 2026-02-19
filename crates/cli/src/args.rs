use alloy::primitives::{Address, TxHash};
use clap::{Parser, Subcommand};
use perpl_sdk::types;

pub(crate) const DEFAULT_MAINNET_RPC_PROVIDER: &str = "https://rpc.monad.xyz";
pub(crate) const DEFAULT_TESTNET_RPC_PROVIDER: &str = "https://testnet-rpc.monad.xyz";
pub(crate) const DEFAULT_RPC_THROTTLING: u32 = 15;

#[derive(Parser, Debug)]
#[command(name = "perpl-cli", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// RPC endpoint to connect to [default: https://rpc.monad.xyz for mainnet, https://testnet-rpc.monad.xyz for testnet]
    #[arg(long, global = true)]
    pub rpc: Option<String>,

    /// Use testnet provider and contract addresses [default: false = mainnet]
    #[arg(long, global = true)]
    pub testnet: bool,

    /// RPC throttling (req/sec) [default: 15 for default RPC providers and
    /// none for custom]
    #[arg(long, global = true)]
    pub rpc_throttle: Option<u32>,

    /// Exchange smart contract address [default: mainnet/testnet smart
    /// contracts]
    #[arg(long, global = true)]
    pub exchange: Option<Address>,

    /// Block number to fetch state at or start tracing from [default: latest
    /// block]
    #[arg(long, global = true)]
    pub block: Option<u64>,

    /// Number of blocks to trace or show [default: unlimited, until terminated
    /// by (Ctrl+C)]
    #[arg(long, global = true)]
    pub num_blocks: Option<u64>,

    /// Account addresses or ID to snaphot/trace/show [default: all accounts for
    /// `snapshot`/`trace`, required for `show account`]
    #[arg(long, global = true)]
    pub account: Vec<types::AccountAddressOrID>,

    /// Perpetual ID to show state/trace for [default: all perpetuals
    /// for `snapshot`/`trace`/`show trades`, required for `show book`]
    #[arg(long, global = true)]
    pub perp: Vec<types::PerpetualId>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Show live state of account, perpetual order book or recent trades
    Show {
        #[command(subcommand)]
        command: ShowCommands,
    },
    /// Take a snapshot of exchange state at a particular block height
    Snapshot,
    /// Take an initial snapshot, then trace all events, then print the final
    /// state
    Trace,
    /// Trace raw events from a particular transaction
    Tx {
        /// Transaction hash to trace
        tx_hash: TxHash,
    },
}

#[derive(Subcommand, Debug)]
pub enum ShowCommands {
    /// Show account state
    Account {
        /// Number of most recent trades to show (0 = don't show trades)
        #[arg(long, default_value_t = 10)]
        num_trades: usize,
    },
    /// Show state of perpetual order book
    Book {
        /// Number of price levels to display (0 = all)
        #[arg(short, long, default_value_t = 10)]
        depth: usize,

        /// Maximum orders to show per level (0 = all)
        #[arg(long, default_value_t = 10)]
        orders_per_level: usize,

        /// Whether to show expired orders
        #[arg(long, default_value_t = false)]
        show_expired: bool,
    },
    /// Show recent trades
    Trades,
}
