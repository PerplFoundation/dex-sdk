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

    /// RPC throttling (req/sec), defaults to 15 for default RPC provider and
    /// none for custom
    #[arg(long, global = true)]
    pub rpc_throttle: Option<u32>,

    /// Exchange smart contract address
    #[arg(long, global = true)]
    pub exchange: Option<Address>,

    /// Block number to fetch state at or start tracing from
    #[arg(long, global = true)]
    pub block: Option<u64>,

    /// Number of blocks to trace or show, defaults to follow until terminated
    /// (Ctrl+C)
    #[arg(long, global = true)]
    pub num_blocks: Option<u64>,

    /// Account addresses or IDs, exactly one required for `show account`.
    #[arg(long, global = true)]
    pub accounts: Vec<types::AccountAddressOrID>,

    /// Perpetual IDs to show state/trace for, exactly one required for `show
    /// book`.
    #[arg(long, global = true)]
    pub perps: Vec<types::PerpetualId>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Take a single snapshot of the exchange state at a particular block and
    /// render it according to the provided display options
    Snapshot,
    /// Take and render an initial snapshot then trace events from the provided
    /// block range and render smart contract events and derived SDK events per
    /// transaction, along with per-block state changes
    Trace,
    /// Render live state of particular account, perpetual order book or trades
    Show {
        #[command(subcommand)]
        command: ShowCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum ShowCommands {
    /// Render live state of particular account
    Account {
        /// Number of most recent trades to show (0 = don't show trades)
        #[arg(long, default_value_t = 10)]
        num_trades: usize,
    },
    /// Render live state of particular perpetual order book
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
    /// Render live trades
    Trades,
}
