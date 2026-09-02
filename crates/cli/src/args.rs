use std::str::FromStr;

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

    /// Account address or ID whose entries - traced events, resting orders,
    /// trades - are painted on a contrasting background [default: no
    /// highlighting]
    #[arg(long, global = true, value_name = "ADDRESS or ACCOUNT_ID")]
    pub highlight: Option<types::AccountAddressOrID>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Trace raw events from a particular block
    Block {
        /// Block number to trace
        block_number: u64,
    },
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
        #[command(flatten)]
        book: BookArgs,
    },
    /// Show how the given market makers are distributed across a perpetual
    /// order book, with their orders colour-coded and their quoting summarised
    Mms {
        /// Market makers to track, each an account address or ID with an
        /// optional label: `ACCOUNT[:LABEL]`. Repeat the argument or separate
        /// entries with commas, eg. `12:Alpha 0xabc..:Beta`
        #[arg(value_name = "ACCOUNT[:LABEL]", required = true, value_delimiter = ',')]
        makers: Vec<MarketMaker>,

        #[command(flatten)]
        book: BookArgs,
    },
    /// Show recent trades
    Trades,
}

/// How much of an order book to render, shared by every command that draws
/// one.
#[derive(clap::Args, Debug)]
pub struct BookArgs {
    /// Number of price levels to display (0 = all)
    #[arg(short, long, default_value_t = 10)]
    pub depth: usize,

    /// Maximum orders to show per level (0 = all)
    #[arg(long, default_value_t = 10)]
    pub orders_per_level: usize,

    /// Whether to show expired orders
    #[arg(long, default_value_t = false)]
    pub show_expired: bool,
}

impl BookArgs {
    /// Price levels to render per side, `None` for all of them.
    pub fn depth(&self) -> Option<usize> { (self.depth > 0).then_some(self.depth) }

    /// Orders to render per price level, `None` for all of them.
    pub fn orders_per_level(&self) -> Option<usize> {
        (self.orders_per_level > 0).then_some(self.orders_per_level)
    }
}

/// A market maker to track, given as an account address or ID with an optional
/// display label after a colon.
#[derive(Clone, Debug)]
pub struct MarketMaker {
    /// Account the maker quotes from.
    pub account: types::AccountAddressOrID,

    /// Label to key the maker's colour by, `None` to fall back to the account
    /// ID it resolves to.
    pub label: Option<String>,
}

impl FromStr for MarketMaker {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Neither an address nor an account ID can contain a colon, so the
        // first one always separates the account from its label
        let (account, label) = s.split_once(':').unwrap_or((s, ""));
        let label = label.trim();
        Ok(Self {
            account: types::AccountAddressOrID::from_str(account.trim())
                .map_err(|err| err.to_string())?,
            label: (!label.is_empty()).then(|| label.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn maker(spec: &str) -> MarketMaker { spec.parse().expect("valid market maker") }

    #[test]
    fn parses_a_market_maker_with_and_without_a_label() {
        let labelled = maker("4638:Alpha");
        assert!(matches!(labelled.account, types::AccountAddressOrID::ID(4638)));
        assert_eq!(labelled.label.as_deref(), Some("Alpha"));

        let bare = maker(" 4638 ");
        assert!(matches!(bare.account, types::AccountAddressOrID::ID(4638)));
        assert_eq!(bare.label, None);

        let address = maker("0x0000000000000000000000000000000000000001:Beta");
        assert!(matches!(address.account, types::AccountAddressOrID::Address(_)));
        assert_eq!(address.label.as_deref(), Some("Beta"));

        assert!("not-an-account".parse::<MarketMaker>().is_err());
    }

    #[test]
    fn accepts_market_makers_repeated_or_comma_separated() {
        let cli = Cli::try_parse_from([
            "perpl-cli",
            "--perp",
            "1",
            "show",
            "mms",
            "4638:Alpha,5022",
            "1743:Gamma",
        ])
        .expect("valid arguments");
        let Commands::Show { command: ShowCommands::Mms { makers, book } } = cli.command else {
            panic!("expected `show mms`");
        };
        assert_eq!(
            makers.iter().map(|m| m.label.clone()).collect::<Vec<_>>(),
            vec![Some("Alpha".to_string()), None, Some("Gamma".to_string())],
        );
        // `show mms` shares the book rendering options with `show book`
        assert_eq!(book.depth(), Some(10));
        assert_eq!(book.orders_per_level(), Some(10));
    }

    #[test]
    fn zero_depth_renders_the_whole_book() {
        let book = BookArgs { depth: 0, orders_per_level: 0, show_expired: false };
        assert_eq!(book.depth(), None);
        assert_eq!(book.orders_per_level(), None);
    }
}
