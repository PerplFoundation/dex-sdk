//! Places an order on a perpetual contract.
//!
//! This is the one command that signs and submits a transaction, so it is
//! deliberately louder than the read commands: it resolves the signer's
//! exchange account, simulates the call, and asks before sending.
//!
//! The order itself is built, quantized, validated and submitted by the SDK -
//! see [`perpl_sdk::types::OrderRequestBuilder`] and [`perpl_sdk::exec`]. What
//! is left here is the terminal side of it: which flag a fault names, what the
//! operator is shown, and whether they agreed to it.

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
use perpl_sdk::{
    Chain,
    error::DexError,
    state::{self, Exchange, Perpetual},
    types::{self, OrderRequest, OrderRequestError, RequestType},
};

use crate::{args::CreateOrderArgs, highlight::Highlights, tx};

/// Builds one order from the command line, simulates it, and - unless this is
/// a dry run - signs and submits it, then traces the resulting transaction.
pub(crate) async fn create<P: Provider + Clone>(
    chain: &Chain,
    provider: P,
    exchange: &Exchange,
    perp_id: types::PerpetualId,
    args: &CreateOrderArgs,
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

    // Everything checkable without the network first - precision, leverage,
    // contradictory flags, a halted exchange - so a mistyped price is reported
    // before an account lookup that would fail for its own reasons
    let request = args
        .to_builder(perp_id)
        .build(exchange)
        .map_err(|err| describe(err, exchange))?;

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
        .get(&perp_id)
        .expect("the request was built against this perpetual");
    print_summary(perp, &request, from, account_id);

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
        .context("simulating the order - it would revert on chain")?;
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

    if !args.yes && !confirm()? {
        println!("{}", "Aborted.".yellow());
        return Ok(());
    }

    let sent = call
        .send()
        .await
        .context("submitting the order transaction")?;
    let tx_hash = sent.tx_hash();
    println!("Submitted {}, waiting for the receipt...", tx_hash.to_string().bright_blue());
    sent.wait()
        .await
        .context("waiting for the order transaction receipt")?;

    // The events say what the exchange actually did with the order - accepted,
    // partially filled, rejected - which the receipt status alone does not
    tx::render(chain, provider, tx_hash, highlights).await
}

/// Renders a rejected order in the terms the caller typed it in: their own
/// flags, and the perpetual's symbol rather than only its ID.
///
/// Anything that is not an order fault - an untracked perpetual, a contract
/// without builder attribution, an RPC failure - already reads well enough as
/// the SDK reports it.
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

    println!("\n{}", format!("**** Order on {} ({})", perp.symbol(), perp.id()).bright_blue());
    println!("  Account         {} (#{})", from, account_id);
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
    // ... and on the client order ID, which it defaulted from the clock where
    // none was given
    println!("  Client order ID {}", request.request_id());
    if perp.is_mark_price_obsolete() {
        println!(
            "  {}",
            "Warning: the mark price is stale, a settling order may be rejected".yellow(),
        );
    }
}

/// Asks the operator to confirm, treating a non-interactive stdin as a refusal
/// rather than an assent - a piped run should pass `--yes` deliberately.
fn confirm() -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("stdin is not a terminal; pass `--yes` to submit without confirmation");
    }
    print!("{}", "Submit this order? [y/N] ".bold());
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}
