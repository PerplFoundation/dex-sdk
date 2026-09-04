use std::{path::PathBuf, str::FromStr};

use alloy::primitives::{Address, TxHash};
use anyhow::Context as _;
use clap::{Parser, Subcommand};
use fastnum::{UD64, decimal::Context};
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
    /// Place an order on a perpetual contract. The only command that signs and
    /// submits a transaction; every other one reads state
    Order {
        #[command(subcommand)]
        command: OrderCommands,
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
pub enum OrderCommands {
    /// Post a single order to the perpetual given by `--perp`
    Create(Box<CreateOrderArgs>),
}

/// Side of the book an order rests on, which together with `--reduce-only`
/// picks the request type the exchange expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Side {
    /// Bid: opens a long, or closes a short when reduce-only
    Buy,
    /// Ask: opens a short, or closes a long when reduce-only
    Sell,
}

impl Side {
    /// Request type this side maps to. The exchange has no side flag - the
    /// request type carries both the direction and whether the order may only
    /// reduce an existing position.
    pub fn request_type(self, reduce_only: bool) -> types::RequestType {
        match (self, reduce_only) {
            (Side::Buy, false) => types::RequestType::OpenLong,
            (Side::Sell, false) => types::RequestType::OpenShort,
            (Side::Sell, true) => types::RequestType::CloseLong,
            (Side::Buy, true) => types::RequestType::CloseShort,
        }
    }
}

/// Everything `order create` needs to build one order descriptor and submit
/// it.
///
/// Price, size, leverage and builder fee are decimals in human units. The
/// perpetual's own scalers convert them to the fixed-point integers the
/// contract stores, so no caller ever writes a scale factor by hand.
#[derive(clap::Args, Debug)]
pub struct CreateOrderArgs {
    /// Side of the book to post on
    #[arg(long, value_enum)]
    pub side: Side,

    /// Order size, a decimal in the perpetual's lot precision, eg. `0.001`
    #[arg(long, visible_alias = "amount", value_name = "DECIMAL", value_parser = decimal)]
    pub size: UD64,

    /// Limit price, a decimal in the perpetual's price precision, eg.
    /// `65432.1`. Required even for an immediate order, where it bounds how
    /// far the fill may run
    #[arg(long, value_name = "DECIMAL", value_parser = decimal)]
    pub price: UD64,

    /// Only reduce an existing position: turns `--side sell` into a close-long
    /// and `--side buy` into a close-short
    #[arg(long, default_value_t = false)]
    pub reduce_only: bool,

    /// Leverage to open the position at, eg. `10` or `12.5`, to at most two
    /// decimal places [default: the perpetual's maximum]
    #[arg(long, value_name = "DECIMAL", value_parser = decimal)]
    pub leverage: Option<UD64>,

    /// Block the order expires at [default: never]
    #[arg(long)]
    pub expiry_block: Option<u64>,

    /// Reject the order rather than let it take liquidity
    #[arg(long, default_value_t = false)]
    pub post_only: bool,

    /// Cancel whatever does not fill immediately
    #[arg(long, default_value_t = false)]
    pub ioc: bool,

    /// Fill the order in full or not at all
    #[arg(long, default_value_t = false)]
    pub fok: bool,

    /// Maximum resting orders this order may match against [default:
    /// unlimited]
    #[arg(long)]
    pub max_matches: Option<u32>,

    /// Additional collateral, in basis points of notional, the exchange may
    /// draw to cover the position's negative unrealized PnL on a fill
    #[arg(long, default_value_t = 0, value_name = "BPS")]
    pub max_neg_pnl_collat_bps: u16,

    /// Client order ID to tag the order with [default: derived from the
    /// current time]
    #[arg(long)]
    pub request_id: Option<u64>,

    /// Builder code to attribute the order to. Needs a contract that supports
    /// builder attribution
    #[arg(long, requires = "builder_fee")]
    pub builder_id: Option<types::BuilderId>,

    /// Fee rate the builder charges on the size this order adds, a decimal
    /// fraction of the traded amount, eg. `0.0001` for 1bp
    #[arg(long, requires = "builder_id", value_name = "DECIMAL", value_parser = decimal)]
    pub builder_fee: Option<UD64>,

    /// Private key of the account to sign with. An argument is visible to
    /// every other process on the machine, so prefer `--private-key-path` or
    /// the environment variable
    #[arg(long, env = "PERPL_PRIVATE_KEY", hide_env_values = true)]
    pub private_key: Option<Secret>,

    /// File to read the signing key from, whitespace trimmed. Takes precedence
    /// over `--private-key` and `PERPL_PRIVATE_KEY`
    #[arg(long, value_name = "PATH")]
    pub private_key_path: Option<PathBuf>,

    /// Gas limit for the transaction [default: estimated]
    #[arg(long)]
    pub gas_limit: Option<u64>,

    /// Build and simulate the order, print what would be sent, then stop
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// Submit without asking for confirmation. Deliberately has no environment
    /// variable: one exported in a shell profile would arm every later order
    /// silently, which is the opposite of what a confirmation is for
    #[arg(long, short = 'y', visible_alias = "auto-confirm", default_value_t = false)]
    pub yes: bool,
}

impl CreateOrderArgs {
    /// Request type the exchange expects for this order.
    pub fn request_type(&self) -> types::RequestType { self.side.request_type(self.reduce_only) }

    /// Signing key, from the file if one was named and otherwise from the
    /// argument or the environment.
    ///
    /// The file wins rather than conflicting with the other two, because
    /// `PERPL_PRIVATE_KEY` is the sort of thing a shell profile exports once:
    /// refusing to run whenever it happens to be set would make
    /// `--private-key-path` unusable in exactly the setup that most wants it.
    pub fn signing_key(&self) -> anyhow::Result<Secret> {
        if let Some(path) = &self.private_key_path {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("reading the signing key from {}", path.display()))?;
            let key = Secret::from_str(&contents).expect("trimming never fails");
            if key.expose().is_empty() {
                anyhow::bail!("{} is empty, it should hold a private key", path.display());
            }
            return Ok(key);
        }
        self.private_key.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "no signing key: pass `--private-key-path`, `--private-key`, or set \
                 PERPL_PRIVATE_KEY",
            )
        })
    }

    /// Builder attribution of the order, `None` when unattributed. Both parts
    /// arrive together or not at all, which clap enforces.
    pub fn builder(&self) -> Option<types::BuilderAttribution> {
        self.builder_id
            .zip(self.builder_fee)
            .map(|(id, fee)| types::BuilderAttribution::new(id, fee))
    }
}

/// A secret held in memory that never renders itself.
///
/// `Debug` and `Display` both print `[redacted]`, so a signing key cannot
/// reach the terminal through a `{:?}` of [`Cli`], a `panic!`, an
/// `anyhow` chain, or a clap error - every one of which would otherwise print
/// the key verbatim, since [`Cli`] derives `Debug`. The value comes out only
/// through [`Secret::expose`], which is deliberately awkward to type and easy
/// to grep for.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The secret itself. Every call site is a place the value could escape -
    /// keep them few, and never pass the result to a formatter.
    pub fn expose(&self) -> &str { self.0.as_str() }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("[redacted]") }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("[redacted]") }
}

impl FromStr for Secret {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> { Ok(Self(s.trim().to_string())) }
}

/// Parses a human-readable decimal - `0.001`, `65432.1` - into the fixed-point
/// type the SDK converts with each perpetual's own scaler.
fn decimal(raw: &str) -> Result<UD64, String> {
    let parsed = UD64::from_str(raw, Context::default())
        .map_err(|err| format!("invalid decimal `{}`: {}", raw, err))?;
    if parsed.is_zero() {
        return Err(format!("`{}` is zero", raw));
    }
    Ok(parsed)
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

    fn create_order(extra: &[&str]) -> Box<CreateOrderArgs> {
        let mut argv = vec![
            "perpl-cli",
            "--perp",
            "1",
            "order",
            "create",
            "--side",
            "buy",
            "--size",
            "0.001",
            "--price",
            "65432.1",
            "--private-key",
            KEY,
        ];
        argv.extend_from_slice(extra);
        let cli = Cli::try_parse_from(argv).expect("valid arguments");
        let Commands::Order { command: OrderCommands::Create(args) } = cli.command else {
            panic!("expected `order create`");
        };
        args
    }

    const KEY: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn parses_price_and_size_as_decimals_not_strings() {
        let args = create_order(&[]);
        // The point of the decimal type: `0.001` keeps its scale rather than
        // arriving as text for a later hand-rolled parse
        assert_eq!(args.size, UD64::from_str("0.001", Context::default()).unwrap());
        assert_eq!(args.price, UD64::from_str("65432.1", Context::default()).unwrap());
        assert_eq!(args.leverage, None);
    }

    #[test]
    fn accepts_amount_as_an_alias_of_size() {
        let cli = Cli::try_parse_from([
            "perpl-cli",
            "--perp",
            "1",
            "order",
            "create",
            "--side",
            "sell",
            "--amount",
            "2.5",
            "--price",
            "10",
            "--private-key",
            KEY,
        ])
        .expect("valid arguments");
        let Commands::Order { command: OrderCommands::Create(args) } = cli.command else {
            panic!("expected `order create`");
        };
        assert_eq!(args.size, UD64::from_str("2.5", Context::default()).unwrap());
    }

    #[test]
    fn rejects_a_non_decimal_or_zero_price() {
        assert!(decimal("not-a-number").is_err());
        assert!(decimal("0").is_err());
        assert!(decimal("0.00").is_err());
        assert!(decimal("0.001").is_ok());
    }

    #[test]
    fn side_and_reduce_only_pick_the_request_type() {
        use types::RequestType::*;
        assert!(matches!(Side::Buy.request_type(false), OpenLong));
        assert!(matches!(Side::Sell.request_type(false), OpenShort));
        assert!(matches!(Side::Sell.request_type(true), CloseLong));
        assert!(matches!(Side::Buy.request_type(true), CloseShort));

        assert!(matches!(create_order(&[]).request_type(), OpenLong));
        assert!(matches!(create_order(&["--reduce-only"]).request_type(), CloseShort));
    }

    #[test]
    fn a_secret_never_renders_itself() {
        let secret = Secret::from_str(KEY).unwrap();
        assert_eq!(format!("{}", secret), "[redacted]");
        assert_eq!(format!("{:?}", secret), "[redacted]");
        // The whole point: `Cli` derives `Debug`, so anything that debug-prints
        // the parsed arguments would otherwise put the key on the terminal
        let printed = format!("{:?}", create_order(&[]));
        assert!(!printed.contains(KEY), "{}", printed);
        assert!(printed.contains("[redacted]"), "{}", printed);
        // ... and it is still the key underneath
        assert_eq!(secret.expose(), KEY);
    }

    #[test]
    fn reads_the_signing_key_from_a_file() {
        let dir = std::env::temp_dir().join(format!("perpl-cli-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("key");
        // Keys land in files with a trailing newline far more often than not
        std::fs::write(&path, format!("{}\n", KEY)).expect("write key");

        let args = create_order(&["--private-key-path", path.to_str().unwrap()]);
        assert_eq!(args.signing_key().unwrap().expose(), KEY);

        // The file wins over the inline argument the helper always passes
        std::fs::write(&path, "").expect("truncate key");
        let err = args.signing_key().expect_err("empty file").to_string();
        assert!(err.contains("is empty"), "{}", err);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_key_file_reports_its_path_and_not_a_key() {
        let args = create_order(&["--private-key-path", "/nonexistent/perpl/key"]);
        let err = format!("{:#}", args.signing_key().expect_err("missing file"));
        assert!(err.contains("/nonexistent/perpl/key"), "{}", err);
        assert!(!err.contains(KEY), "{}", err);
    }

    #[test]
    fn requires_a_key_from_somewhere() {
        // No `--private-key`, no `--private-key-path`; the env var is the
        // remaining source and is not set in the test process
        let parsed = Cli::try_parse_from([
            "perpl-cli",
            "--perp",
            "1",
            "order",
            "create",
            "--side",
            "buy",
            "--size",
            "1",
            "--price",
            "1",
        ]);
        // Absent the env var clap has nothing to fill the argument with, so
        // either clap rejects it or `signing_key` does - both are refusals
        if let Ok(cli) = parsed {
            let Commands::Order { command: OrderCommands::Create(args) } = cli.command else {
                panic!("expected `order create`");
            };
            if std::env::var("PERPL_PRIVATE_KEY").is_err() {
                assert!(args.signing_key().is_err());
            }
        }
    }

    #[test]
    fn auto_confirm_is_an_alias_of_yes() {
        assert!(!create_order(&[]).yes);
        assert!(create_order(&["--yes"]).yes);
        assert!(create_order(&["-y"]).yes);
        assert!(create_order(&["--auto-confirm"]).yes);
    }

    #[test]
    fn builder_attribution_needs_both_halves() {
        assert_eq!(create_order(&[]).builder(), None);

        let attributed = create_order(&["--builder-id", "7", "--builder-fee", "0.0001"]);
        let builder = attributed.builder().expect("builder attribution");
        assert_eq!(builder.builder_id(), 7);
        assert_eq!(builder.fee(), UD64::from_str("0.0001", Context::default()).unwrap());

        // An id without a rate, or a rate without an id, is a half-specified
        // envelope the contract would reject
        assert!(
            Cli::try_parse_from([
                "perpl-cli",
                "--perp",
                "1",
                "order",
                "create",
                "--side",
                "buy",
                "--size",
                "1",
                "--price",
                "1",
                "--private-key",
                KEY,
                "--builder-id",
                "7",
            ])
            .is_err()
        );
    }
}
