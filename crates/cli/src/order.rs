//! Places an order on a perpetual contract.
//!
//! This is the one command that signs and submits a transaction, so it is
//! deliberately louder than the read commands: it resolves the signer's
//! exchange account, quantizes price and size against the perpetual's own
//! precision, simulates the call, and asks before sending.

use std::{
    io::{IsTerminal, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, Bytes, utils::format_units},
    providers::{Provider, ProviderBuilder},
    rpc::client::RpcClient,
    signers::local::PrivateKeySigner,
    transports::layers::RetryBackoffLayer,
};
use anyhow::{Context as _, bail};
use colored::Colorize;
use fastnum::UD64;
use perpl_sdk::{
    Chain,
    abi::{
        dex::{self, Exchange::OrderDesc},
        errors::Exchange::ExchangeErrors,
    },
    error::{DexError, ProviderError, RevertReason},
    num,
    state::{Exchange, Perpetual},
    types::{self, OrderRequest},
};

use crate::{args::CreateOrderArgs, highlight::Highlights, tx};

/// Builds one order from the command line, simulates it, and - unless this is
/// a dry run - signs and submits it, then traces the resulting transaction.
pub(crate) async fn create<P: Provider + Clone>(
    chain: &Chain,
    provider: P,
    rpc: &str,
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

    let perp = exchange
        .perpetuals()
        .get(&perp_id)
        .ok_or_else(|| anyhow::anyhow!("perpetual {} is not tracked by the snapshot", perp_id))?;

    // Fail before signing anything on the states the contract would reject
    // anyway, where the revert reason is far less legible than this
    if exchange.is_halted() {
        bail!("the exchange is halted, no order can be placed");
    }
    if perp.is_paused() {
        bail!("perpetual {} ({}) is paused, no order can be placed", perp_id, perp.symbol());
    }

    // Everything checkable without the network first, so a mistyped price is
    // reported before an account lookup that would fail for its own reasons
    let request = build_request(perp, exchange, args, perp_id)?;
    let (desc, extension) = prepare(exchange, &request, args)?;
    let account_id = resolve_account(chain, provider.clone(), from).await?;

    print_summary(perp, args, &request, &desc, from, account_id);

    // A wallet-bearing provider is built only for this command; every read
    // above went through the shared read-only one
    let wallet_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_client(
            RpcClient::builder()
                .layer(RetryBackoffLayer::new(10, 100, 200))
                .connect(rpc)
                .await
                .context("connecting to RPC to submit the order")?,
        );
    let instance = dex::Exchange::new(chain.exchange(), &wallet_provider);

    // `execOrdersV2` carries the builder envelope; the V1 entrypoint has
    // nothing to put it in, so a contract without V2 support cannot honour an
    // attributed order. The two entrypoints are distinct call types, so they
    // meet as a transaction request and share every step from here on
    let mut tx = if extension.is_empty() && !exchange.features().builder_attribution() {
        instance
            .execOrders(vec![desc], true)
            .into_transaction_request()
    } else {
        instance
            .execOrdersV2(vec![desc], true, vec![extension])
            .into_transaction_request()
    }
    .with_from(from);
    if let Some(gas) = args.gas_limit {
        tx.set_gas_limit(gas);
    }

    // A revert here is the common outcome of a bad order - insufficient
    // collateral, a stale mark, a reduce-only order with nothing to reduce -
    // so it is decoded to the contract's own error name rather than a blob
    wallet_provider
        .call(tx.clone())
        .await
        .map_err(|err| DexError::Provider(err.into()))
        .context("simulating the order - it would revert on chain")?;
    println!("{}", "Simulated without reverting.".green());

    if args.dry_run {
        println!(
            "\n{}\n  {}",
            "Dry run, nothing was sent. Calldata:".yellow(),
            tx.input.input().cloned().unwrap_or_default(),
        );
        return Ok(());
    }

    if !args.yes && !confirm()? {
        println!("{}", "Aborted.".yellow());
        return Ok(());
    }

    let pending = wallet_provider
        .send_transaction(tx)
        .await
        .context("submitting the order transaction")?;
    let tx_hash = *pending.tx_hash();
    println!("Submitted {}, waiting for the receipt...", tx_hash.to_string().bright_blue());

    let receipt = pending
        .get_receipt()
        .await
        .context("waiting for the order transaction receipt")?;
    if !receipt.status() {
        bail!("order transaction {} reverted on chain", tx_hash);
    }

    // The events say what the exchange actually did with the order - accepted,
    // partially filled, rejected - which the receipt status alone does not
    tx::render(chain, provider, tx_hash, highlights).await
}

/// Assembles the order request from the command line, checking that price and
/// size survive the perpetual's precision unchanged.
fn build_request(
    perp: &Perpetual,
    exchange: &Exchange,
    args: &CreateOrderArgs,
    perp_id: types::PerpetualId,
) -> anyhow::Result<OrderRequest> {
    let price = quantize(args.price, perp.price_converter(), "--price")?;
    let size = quantize(args.size, perp.size_converter(), "--size")?;
    // Zero is the exchange's "use the maximum" sentinel, so an omitted
    // leverage is spelled out rather than left to resolve silently
    let leverage = match args.leverage {
        Some(leverage) => quantize(leverage, perp.leverage_converter(), "--leverage")?,
        None => perp.initial_margin(),
    };
    if leverage > perp.initial_margin() {
        bail!(
            "leverage {} exceeds the maximum of {} on perpetual {} ({})",
            leverage,
            perp.initial_margin(),
            perp_id,
            perp.symbol(),
        );
    }
    if args.fok && args.post_only {
        bail!(
            "--fok and --post-only contradict each other: a post-only order never fills on entry"
        );
    }
    if let Some(builder) = args.builder()
        && !exchange.features().builder_attribution()
    {
        bail!(
            "the deployed contract does not support builder attribution, so builder {} cannot be \
             carried",
            builder.builder_id(),
        );
    }

    let mut request = OrderRequest::new(
        args.request_id.unwrap_or_else(default_request_id),
        perp_id,
        args.request_type(),
        // A new order carries no exchange order ID; the exchange assigns one
        None,
        price,
        size,
        args.expiry_block,
        args.post_only,
        args.fok,
        args.ioc,
        args.max_matches,
        leverage,
        // Only `Change` requests are conditioned on a last execution block
        None,
        // `amountCNS` carries collateral for `IncreasePositionCollateral`, not
        // for a posted order
        None,
        args.max_neg_pnl_collat_bps,
    );
    if let Some(builder) = args.builder() {
        request = request.with_builder(builder);
    }
    Ok(request)
}

/// Turns the request into the descriptor the contract takes, plus the V2 order
/// extension envelope - empty for an unattributed order.
fn prepare(
    exchange: &Exchange,
    request: &OrderRequest,
    args: &CreateOrderArgs,
) -> anyhow::Result<(OrderDesc, Bytes)> {
    if args.builder().is_some() || exchange.features().builder_attribution() {
        Ok(request
            .prepare_v2(exchange)
            .context("preparing the order for the V2 entrypoint")?)
    } else {
        Ok((request.prepare(exchange), Bytes::new()))
    }
}

/// Rescales `value` to `converter`'s precision, rejecting anything that would
/// lose digits. Silently truncating a price the caller typed is the one
/// failure mode worth being noisy about.
fn quantize(value: UD64, converter: num::Converter, flag: &str) -> anyhow::Result<UD64> {
    let rescaled = value.rescale(converter.decimals() as i16);
    if rescaled != value {
        bail!(
            "{} {} carries more precision than perpetual's {} decimal place(s) allows; it would \
             become {}",
            flag,
            value,
            converter.decimals(),
            rescaled,
        );
    }
    Ok(rescaled)
}

/// Resolves the signing address to its exchange account, which must already
/// exist - the exchange opens accounts on deposit, not on order.
async fn resolve_account<P: Provider + Clone>(
    chain: &Chain,
    provider: P,
    address: Address,
) -> anyhow::Result<types::AccountId> {
    match dex::Exchange::new(chain.exchange(), provider)
        .getAccountByAddr(address)
        .call()
        .await
    {
        Ok(account) => Ok(account.accountId.to()),
        // The exchange reverts rather than returning zero for an address it has
        // never opened an account for. That is the common mistake here, so it
        // gets the instruction rather than a decoded error name
        Err(err) => match DexError::Provider(err.into()) {
            DexError::Provider(ProviderError::Reverted(reason))
                if matches!(
                    *reason,
                    RevertReason::Known(ExchangeErrors::AccountDoesNotExist(_))
                ) =>
            {
                bail!(
                    "{} has no exchange account; deposit collateral before placing an order",
                    address,
                )
            },
            err => Err(err).with_context(|| format!("resolving exchange account of {}", address)),
        },
    }
}

/// Client order ID for a request that did not name one. Milliseconds since the
/// epoch are monotonic enough to keep a session's orders distinguishable in
/// the event stream.
fn default_request_id() -> types::RequestId {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// Prints what is about to be signed, in the same human units the caller typed.
fn print_summary(
    perp: &Perpetual,
    args: &CreateOrderArgs,
    request: &OrderRequest,
    desc: &OrderDesc,
    from: Address,
    account_id: types::AccountId,
) {
    let flags = [
        args.post_only.then_some("post-only"),
        args.ioc.then_some("immediate-or-cancel"),
        args.fok.then_some("fill-or-kill"),
        args.reduce_only.then_some("reduce-only"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    println!("\n{}", format!("**** Order on {} ({})", perp.symbol(), perp.id()).bright_blue());
    println!("  Account         {} (#{})", from, account_id);
    println!("  Type            {:?}", args.request_type());
    println!("  Size            {}", args.size);
    println!("  Price           {}", args.price);
    println!("  Notional        {}", args.size * args.price);
    println!("  Leverage        {}", desc_leverage(perp, desc));
    println!("  Mark / last     {} / {}", perp.mark_price(), perp.last_price());
    if !flags.is_empty() {
        println!("  Flags           {}", flags.join(", "));
    }
    if let Some(expiry) = args.expiry_block {
        println!("  Expires at      block {}", expiry);
    }
    if let Some(builder) = request.builder() {
        println!("  Builder         {} at {}", builder.builder_id(), builder.fee());
    }
    println!(
        "  Client order ID {}",
        // The descriptor is the authority here: it carries the ID that was
        // defaulted when none was given
        desc.orderDescId,
    );
    if perp.is_mark_price_obsolete() {
        println!(
            "  {}",
            "Warning: the mark price is stale, a settling order may be rejected".yellow(),
        );
    }
}

/// Renders the leverage actually encoded into the descriptor.
fn desc_leverage(perp: &Perpetual, desc: &OrderDesc) -> String {
    format_units(desc.leverageHdths, perp.leverage_converter().decimals())
        .unwrap_or_else(|_| desc.leverageHdths.to_string())
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

#[cfg(test)]
mod tests {
    use fastnum::decimal::Context;

    use super::*;

    fn dec(raw: &str) -> UD64 { UD64::from_str(raw, Context::default()).expect("valid decimal") }

    #[test]
    fn accepts_a_value_that_fits_the_perpetual_precision() {
        // Testnet BTC prices to one decimal place, sizes to five
        let price = num::Converter::new(1);
        assert_eq!(quantize(dec("65432.1"), price, "--price").unwrap(), dec("65432.1"));

        let size = num::Converter::new(5);
        assert_eq!(quantize(dec("0.001"), size, "--size").unwrap(), dec("0.001"));
    }

    #[test]
    fn accepts_a_value_coarser_than_the_perpetual_precision() {
        // A whole-number price is not over-precise, so padding it out to the
        // contract's scale must not read as a loss of digits
        let price = num::Converter::new(1);
        assert_eq!(quantize(dec("65432"), price, "--price").unwrap(), dec("65432"));

        let mon = num::Converter::new(6);
        assert_eq!(quantize(dec("0.05"), mon, "--price").unwrap(), dec("0.05"));
    }

    #[test]
    fn rejects_a_value_the_perpetual_would_silently_truncate() {
        let price = num::Converter::new(1);
        let err = quantize(dec("65432.123456"), price, "--price")
            .expect_err("over-precise price")
            .to_string();
        // The message has to name what the value would have become, or the
        // caller cannot tell how much precision they lost
        assert!(err.contains("65432.1"), "{}", err);
        assert!(err.contains("--price"), "{}", err);

        let size = num::Converter::new(5);
        assert!(quantize(dec("0.0000001"), size, "--size").is_err());
    }
}
