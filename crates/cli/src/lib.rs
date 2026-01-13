mod account;
pub mod args;
mod book;
mod snapshot;
mod trace;
mod trades;

use std::time::Duration;

use alloy::{
    primitives::Address,
    providers::{Provider, ProviderBuilder},
    rpc::{client::RpcClient, types::BlockId},
    transports::layers::{RetryBackoffLayer, ThrottleLayer},
};
use anyhow::Context;
use args::Cli;
use perpl_sdk::{Chain, state::SnapshotBuilder};
use tokio_util::sync::CancellationToken;

use crate::args::{Commands, ShowCommands};

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let client = if cli.rpc == args::DEFAULT_RPC_PROVIDER || cli.rpc_throttle.is_some() {
        // Apply throttling with default RPC
        RpcClient::builder()
            .layer(ThrottleLayer::new(cli.rpc_throttle.unwrap_or(args::DEFAULT_RPC_THROTTLING)))
            .layer(RetryBackoffLayer::new(10, 100, 200))
            .connect(&cli.rpc)
            .await
            .context("connecting to RPC")?
    } else {
        RpcClient::builder()
            .layer(RetryBackoffLayer::new(10, 100, 200))
            .connect(&cli.rpc)
            .await
            .context("connecting to RPC")?
    };
    client.set_poll_interval(Duration::from_millis(100));
    let provider = ProviderBuilder::new().connect_client(client);

    if let Some(unknown_perp) = cli
        .perp
        .iter()
        .find(|perp_id| !Chain::testnet().perpetuals().contains(perp_id))
    {
        return Err(anyhow::anyhow!("unknown perpetual ID: {}", unknown_perp));
    }

    let chain = Chain::custom(
        provider.get_chain_id().await?,
        Address::ZERO,
        0,
        cli.exchange.unwrap_or(Chain::testnet().exchange()),
        if !cli.perp.is_empty() {
            cli.perp.clone()
        } else {
            Chain::testnet().perpetuals().to_vec()
        },
    );

    let mut builder = SnapshotBuilder::new(&chain, provider.clone());
    if let Some(block) = cli.block {
        builder = builder.at_block(BlockId::number(block));
    }

    if !cli.account.is_empty() {
        builder = builder.with_accounts(cli.account.clone());
    } else {
        builder = builder.with_all_positions();
    }

    let builder = match &cli.command {
        Commands::Snapshot | Commands::Trace => Some(builder),
        Commands::Show { command } => match command {
            ShowCommands::Account { num_trades: _ } => {
                if cli.account.len() != 1 {
                    return Err(anyhow::anyhow!(
                        "exactly one account should be provided, see `--account`"
                    ));
                }
                Some(builder)
            },
            ShowCommands::Book { depth: _, orders_per_level: _, show_expired: _ } => {
                if cli.perp.len() != 1 {
                    return Err(anyhow::anyhow!(
                        "exactly one perp should be provided, see `--perp`"
                    ));
                }
                Some(builder)
            },
            ShowCommands::Trades => None,
        },
    };

    let exchange = if let Some(builder) = builder {
        Some(
            builder
                .build()
                .await
                .context("building exchange snapshot")?,
        )
    } else {
        None
    };

    let cancellation_signal = CancellationToken::new();
    let cancellation_token = cancellation_signal.child_token();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install CTRL+C signal handler");
        cancellation_signal.cancel();
    });

    match &cli.command {
        Commands::Snapshot => snapshot::render(exchange.unwrap()),
        Commands::Trace => {
            trace::render(chain, provider, exchange.unwrap(), cli.num_blocks, cancellation_token)
                .await?
        },
        Commands::Show { command } => match command {
            ShowCommands::Account { num_trades } => {
                account::render(
                    chain,
                    provider,
                    exchange.unwrap(),
                    cli.num_blocks,
                    *num_trades,
                    cancellation_token,
                )
                .await?
            },
            ShowCommands::Book { depth, orders_per_level, show_expired } => {
                book::render(
                    chain,
                    provider,
                    exchange.unwrap(),
                    *depth,
                    *orders_per_level,
                    *show_expired,
                    cli.num_blocks,
                    cancellation_token,
                )
                .await?
            },
            ShowCommands::Trades => {
                trades::render(chain, provider, cli.num_blocks, cancellation_token).await?
            },
        },
    }

    Ok(())
}
