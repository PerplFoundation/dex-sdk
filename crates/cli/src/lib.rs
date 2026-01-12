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
    transports::layers::RetryBackoffLayer,
};
use anyhow::Context;
use args::Cli;
use perpl_sdk::{Chain, state::SnapshotBuilder};
use tokio_util::sync::CancellationToken;

use crate::args::{Commands, ShowCommands};

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let client = RpcClient::builder()
        .layer(RetryBackoffLayer::new(10, 100, 200))
        .connect(&cli.rpc)
        .await
        .context("connecting to RPC")?;
    client.set_poll_interval(Duration::from_millis(250));
    let provider = ProviderBuilder::new().connect_client(client);

    if let Some(unknown_perp) = cli
        .perps
        .iter()
        .find(|perp_id| !Chain::testnet().perpetuals().contains(perp_id))
    {
        return Err(anyhow::anyhow!("unknown perpetual ID: {}", unknown_perp));
    }

    let mut chain = Chain::custom(
        provider.get_chain_id().await?,
        Address::ZERO,
        0,
        cli.exchange.unwrap_or(Chain::testnet().exchange()),
        if !cli.perps.is_empty() { cli.perps } else { Chain::testnet().perpetuals().to_vec() },
    );

    let mut builder = SnapshotBuilder::new(&chain, provider.clone());
    if let Some(block) = cli.block {
        builder = builder.at_block(BlockId::number(block));
    }

    if !cli.accounts.is_empty() {
        builder = builder.with_accounts(cli.accounts);
    } else {
        builder = builder.with_all_positions();
    }

    let builder = match &cli.command {
        Commands::Snapshot | Commands::Trace => Some(builder),
        Commands::Show { command } => match command {
            ShowCommands::Account { account, num_trades: _ } => {
                Some(builder.with_accounts(vec![*account]))
            },
            ShowCommands::Book { perp, depth: _, orders_per_level: _, show_expired: _ } => {
                if !Chain::testnet().perpetuals().contains(perp) {
                    return Err(anyhow::anyhow!("unknown perpetual ID: {}", perp));
                }
                chain = Chain::custom(
                    chain.chain_id(),
                    chain.collateral_token(),
                    chain.deployed_at_block(),
                    chain.exchange(),
                    vec![*perp],
                );
                Some(builder.with_perpetuals(vec![*perp]))
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
            ShowCommands::Account { account: _, num_trades } => {
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
            ShowCommands::Book { perp, depth, orders_per_level, show_expired } => {
                book::render(
                    chain,
                    provider,
                    exchange.unwrap(),
                    *perp,
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
