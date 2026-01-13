use alloy::primitives::Address;
use clap::{Parser, Subcommand};
use perpl_sdk::types;

pub(crate) const DEFAULT_RPC_PROVIDER: &str = "https://testnet-rpc.monad.xyz";
pub(crate) const DEFAULT_RPC_THROTTLING: u32 = 15;

#[derive(Parser, Debug)]
#[command(name = "perpl-cli", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// RPC endpoint to connect to
    #[arg(long, global = true, default_value_t = DEFAULT_RPC_PROVIDER.to_string() )]
    pub rpc: String,

    /// RPC throttling (req/sec) [default: 15 for default RPC provider and
    /// none for custom]
    #[arg(long, global = true)]
    pub rpc_throttle: Option<u32>,

    /// Exchange smart contract address [default: testnet smart contract]
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
    /// Take a snapshot of exchange state at a particular block height
    Snapshot,
    /// Take an initial snapshot, then trace all events, then print the final
    /// state
    Trace,
    /// Show live state of account, perpetual order book or recent trades
    Show {
        #[command(subcommand)]
        command: ShowCommands,
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
