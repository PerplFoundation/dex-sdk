//! Places, cancels and changes orders on a perpetual contract.
//!
//! These are the only commands that sign and submit a transaction, so they are
//! deliberately louder than the read commands: each resolves the signer's
//! exchange account, prints what it is about to do, simulates the call, and
//! asks before sending.
//!
//! The requests themselves are built, quantized, validated and submitted by
//! the SDK - see [`perpl_sdk::types::OrderRequestBuilder`] and
//! [`perpl_sdk::exec`]. What is left here is the terminal side of it: which
//! flag a fault names, what the operator is shown, and whether they agreed to
//! it. All three commands share one submission path, because to the exchange
//! they are the same operation with a different request type.

use std::io::{IsTerminal, Write};

use alloy::{
    network::EthereumWallet,
    primitives::Address,
    providers::{Provider, ProviderBuilder},
    rpc::types::BlockId,
    signers::local::PrivateKeySigner,
};
use anyhow::{Context as _, bail};
use colored::Colorize;
use fastnum::UD64;
use perpl_sdk::{
    Chain,
    error::DexError,
    state::{self, Exchange, Order, Perpetual},
    types::{self, OrderRequest, OrderRequestError, RequestType},
};

use crate::{
    args::{OrderCommands, OrderTxArgs},
    highlight::Highlights,
    tx,
};

/// Builds the request the command describes, then submits it.
pub(crate) async fn run<P: Provider + Clone>(
    chain: &Chain,
    provider: P,
    exchange: &Exchange,
    perp_id: types::PerpetualId,
    command: &OrderCommands,
    highlights: &Highlights,
) -> anyhow::Result<()> {
    let (builder, tx_args) = match command {
        OrderCommands::Create(args) => (args.to_builder(perp_id), &args.tx),
        OrderCommands::Cancel(args) => (args.to_builder(perp_id), &args.tx),
        OrderCommands::Change(args) => (args.to_builder(perp_id), &args.tx),
    };
    // Everything checkable without the network first - precision, leverage,
    // contradictory flags, whether the order is even on the book - so a
    // mistyped price is reported before an account lookup that would fail for
    // its own reasons
    let request = builder
        .build(exchange)
        .map_err(|err| describe(err, exchange))?;

    submit(chain, provider, exchange, &request, tx_args, highlights).await
}

/// Signs and submits one request, simulating it and - unless this is a dry run
/// - asking first, then tracing the resulting transaction.
async fn submit<P: Provider + Clone>(
    chain: &Chain,
    provider: P,
    exchange: &Exchange,
    request: &OrderRequest,
    args: &OrderTxArgs,
    highlights: &Highlights,
) -> anyhow::Result<()> {
    // The error deliberately carries no detail from the key itself - a parse
    // failure that echoed the input would put it on the terminal
    let signer: PrivateKeySigner = args
        .signing_key()?
        .expose()
        .parse()
        .map_err(|_| anyhow::anyhow!("the signing key is not a valid private key"))?;
    let from = signer.address();

    let account_id = state::account_id_by_address(chain, provider.clone(), from, BlockId::latest())
        .await
        .with_context(|| format!("resolving exchange account of {}", from))?
        // The exchange opens an account on deposit, not on order, and that is
        // the common mistake here
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} has no exchange account; deposit collateral before placing an order",
                from,
            )
        })?;

    let perp = exchange
        .perpetuals()
        .get(&request.perp_id())
        .expect("the request was built against this perpetual");
    print_summary(perp, request, from, account_id);

    // The signing path is the shared provider with a wallet stacked on top of
    // it, so it keeps the throttling, retry and poll-interval settings the read
    // commands were given rather than dialling a second connection. The fillers
    // sit above the wallet - they fill nonce, gas and chain ID before it signs -
    // and the inner provider only ever sees the signed envelope, so its own
    // fillers stay out of the way
    let wallet_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_provider(provider.clone());

    let mut call = request.call(exchange, wallet_provider, from)?;
    if let Some(gas) = args.gas_limit {
        call = call.with_gas_limit(gas);
    }

    call.simulate()
        .await
        .context("simulating the request - it would revert on chain")?;
    println!("{}", "Simulated without reverting.".green());

    if args.dry_run {
        println!(
            "\n{}\n  {}",
            "Dry run, nothing was sent. Calldata:".yellow(),
            call.transaction()
                .input
                .input()
                .cloned()
                .unwrap_or_default(),
        );
        return Ok(());
    }

    if !args.yes && !confirm(request.request_type())? {
        println!("{}", "Aborted.".yellow());
        return Ok(());
    }

    let sent = call.send().await.context("submitting the transaction")?;
    let tx_hash = sent.tx_hash();
    println!("Submitted {}, waiting for the receipt...", tx_hash.to_string().bright_blue());
    sent.wait()
        .await
        .context("waiting for the transaction receipt")?;

    // The events say what the exchange actually did with the request -
    // accepted, partially filled, rejected - which the receipt status alone
    // does not
    tx::render(chain, provider, tx_hash, highlights).await
}

/// Renders a rejected request in the terms the caller typed it in: their own
/// flags, and the perpetual's symbol rather than only its ID.
///
/// Anything that is not a request fault - an untracked perpetual, an order
/// that is not on the book, a contract without builder attribution, an RPC
/// failure - already reads well enough as the SDK reports it.
fn describe(err: DexError, exchange: &Exchange) -> anyhow::Error {
    let DexError::OrderRequest(fault) = &err else {
        return err.into();
    };
    match fault {
        // The SDK's message opens with the field it faulted on - `price`,
        // `size`, `leverage` - which is the flag the caller typed, less the
        // dashes
        OrderRequestError::Precision { .. } => anyhow::anyhow!("--{}", fault),
        OrderRequestError::ExchangeHalted => anyhow::anyhow!("{}, no order can be placed", fault),
        OrderRequestError::PerpetualPaused(perp_id) => {
            anyhow::anyhow!("{} ({}), no order can be placed", fault, symbol(exchange, *perp_id))
        },
        OrderRequestError::LeverageTooHigh { perp, .. } => {
            anyhow::anyhow!("{} ({})", fault, symbol(exchange, *perp))
        },
        // The one contradictory pair the exchange has; the explanation is
        // specific to it, so a future pair falls through to the SDK's message
        OrderRequestError::ContradictoryFlags("post-only", "fill-or-kill") => anyhow::anyhow!(
            "--post-only and --fok contradict each other: a post-only order never fills on entry",
        ),
        OrderRequestError::NothingToChange(order_id) => anyhow::anyhow!(
            "nothing to change about order {}: pass `--price`, `--size` or `--expiry-block`",
            order_id,
        ),
        OrderRequestError::ChangeExpiredOrderNeedsNewExpiry(_) => {
            anyhow::anyhow!("{}, pass `--expiry-block`", fault)
        },
        _ => err.into(),
    }
}

/// Symbol of a perpetual the snapshot tracks, for a message that would
/// otherwise name only its ID.
fn symbol(exchange: &Exchange, perp_id: types::PerpetualId) -> String {
    exchange
        .perpetuals()
        .get(&perp_id)
        .map(Perpetual::symbol)
        .unwrap_or_else(|| perp_id.to_string())
}

/// Prints what is about to be signed, in the same human units the caller typed.
fn print_summary(
    perp: &Perpetual,
    request: &OrderRequest,
    from: Address,
    account_id: types::AccountId,
) {
    // A cancel or a change names an order the snapshot already holds, and what
    // it holds is what the request is about to move away from
    let resting = request
        .order_id()
        .and_then(|order_id| perp.l3_book().get_order(order_id))
        .map(|order| &**order);

    let what = match (request.request_type(), request.order_id()) {
        (RequestType::Cancel, Some(order_id)) => format!("Cancel order #{}", order_id),
        (RequestType::Change, Some(order_id)) => format!("Change order #{}", order_id),
        _ => "Order".to_string(),
    };
    println!("\n{}", format!("**** {} on {} ({})", what, perp.symbol(), perp.id()).bright_blue());
    println!("  Account         {} (#{})", from, account_id);

    match request.request_type() {
        RequestType::Cancel => {
            println!("  Resting         {} @ {}", request.size(), request.price());
        },
        RequestType::Change => {
            println!("  Price           {}", amendment(resting.map(Order::price), request.price()));
            println!("  Size            {}", amendment(resting.map(Order::size), request.size()));
            if let Some(expiry) = request.expiry_block() {
                println!(
                    "  Expires at      {}",
                    amendment_of(resting.map(Order::expiry_block), expiry),
                );
            }
            if let Some(block) = request.last_exec_block() {
                println!("  Only if unfilled since block {}", block);
            }
        },
        _ => print_order(perp, request),
    }

    // The request is the authority on the client order ID, which it defaulted
    // from the clock where none was given
    println!("  Client order ID {}", request.request_id());
    if !matches!(request.request_type(), RequestType::Cancel) && perp.is_mark_price_obsolete() {
        println!(
            "  {}",
            "Warning: the mark price is stale, a settling order may be rejected".yellow(),
        );
    }
}

/// The order-placing half of the summary.
fn print_order(perp: &Perpetual, request: &OrderRequest) {
    let flags = [
        request.post_only().then_some("post-only"),
        request
            .immediate_or_cancel()
            .then_some("immediate-or-cancel"),
        request.fill_or_kill().then_some("fill-or-kill"),
        matches!(request.request_type(), RequestType::CloseLong | RequestType::CloseShort)
            .then_some("reduce-only"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    println!("  Type            {:?}", request.request_type());
    println!("  Size            {}", request.size());
    println!("  Price           {}", request.price());
    println!("  Notional        {}", request.size() * request.price());
    // The request is the authority on leverage: it carries the perpetual's
    // maximum where the caller named none
    println!("  Leverage        {}", request.leverage());
    println!("  Mark / last     {} / {}", perp.mark_price(), perp.last_price());
    if !flags.is_empty() {
        println!("  Flags           {}", flags.join(", "));
    }
    if let Some(expiry) = request.expiry_block() {
        println!("  Expires at      block {}", expiry);
    }
    if let Some(builder) = request.builder_attribution() {
        println!("  Builder         {} at {}", builder.builder_id(), builder.fee());
    }
}

/// Renders a change as what it moves away from, so an amendment that in fact
/// amends nothing reads as such.
fn amendment(from: Option<UD64>, to: UD64) -> String {
    match from {
        Some(from) if from != to => format!("{} -> {}", from, to),
        Some(from) => format!("{} (unchanged)", from),
        None => to.to_string(),
    }
}

/// Same, for the expiry block, where zero is the contract's "never".
fn amendment_of(from: Option<u64>, to: u64) -> String {
    match from {
        Some(0) | None => format!("block {}", to),
        Some(from) if from != to => format!("block {} -> {}", from, to),
        Some(from) => format!("block {} (unchanged)", from),
    }
}

/// Asks the operator to confirm, treating a non-interactive stdin as a refusal
/// rather than an assent - a piped run should pass `--yes` deliberately.
fn confirm(r#type: RequestType) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("stdin is not a terminal; pass `--yes` to submit without confirmation");
    }
    let what = match r#type {
        RequestType::Cancel => "Submit this cancellation? [y/N] ",
        RequestType::Change => "Submit this change? [y/N] ",
        _ => "Submit this order? [y/N] ",
    };
    print!("{}", what.bold());
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}
