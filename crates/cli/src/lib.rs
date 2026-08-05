mod account;
pub mod args;
mod block;
mod book;
mod snapshot;
mod trace;
mod trades;
mod tx;

use std::time::Duration;

use alloy::{
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
    let chain = if cli.testnet { Chain::testnet() } else { Chain::mainnet() };
    let (rpc, default) = cli.rpc.map(|rpc| (rpc, false)).unwrap_or(if cli.testnet {
        (args::DEFAULT_TESTNET_RPC_PROVIDER.to_string(), true)
    } else {
        (args::DEFAULT_MAINNET_RPC_PROVIDER.to_string(), true)
    });
    let client = if default || cli.rpc_throttle.is_some() {
        // Apply throttling with default RPC
        RpcClient::builder()
            .layer(ThrottleLayer::new(cli.rpc_throttle.unwrap_or(args::DEFAULT_RPC_THROTTLING)))
            .layer(RetryBackoffLayer::new(10, 100, 200))
            .connect(&rpc)
            .await
            .context("connecting to RPC")?
    } else {
        RpcClient::builder()
            .layer(RetryBackoffLayer::new(10, 100, 200))
            .connect(&rpc)
            .await
            .context("connecting to RPC")?
    };
    client.set_poll_interval(Duration::from_millis(100));
    let provider = ProviderBuilder::new().connect_client(client);

    // An empty perpetual list makes the SDK track every contract listed on the
    // exchange, discovered on-chain
    let chain = Chain::custom(
        provider.get_chain_id().await?,
        chain.collateral_token(),
        chain.deployed_at_block(),
        cli.exchange.unwrap_or(chain.exchange()),
        cli.perp.clone(),
    )
    // Carried over: `custom` starts with no exclusions, but the base chain's
    // apply just as much to a custom exchange address on the same network
    .with_excluded_perpetuals(chain.excluded_perpetuals().to_vec());

    if !cli.perp.is_empty() {
        let listed = perpl_sdk::state::listed_perpetuals(
            &chain,
            provider.clone(),
            cli.block.map(BlockId::number).unwrap_or(BlockId::safe()),
        )
        .await
        .context("discovering listed perpetuals")?;
        if let Some(unknown_perp) = cli.perp.iter().find(|perp_id| !listed.contains(perp_id)) {
            // Discovery leaves the chain's excluded contracts out, so say which
            // of the two it is
            if chain.excluded_perpetuals().contains(unknown_perp) {
                return Err(anyhow::anyhow!(
                    "perpetual ID {} is excluded from indexing for this chain",
                    unknown_perp,
                ));
            }
            return Err(anyhow::anyhow!(
                "unknown perpetual ID: {}, listed: {:?}",
                unknown_perp,
                listed,
            ));
        }
    }

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
        Commands::Block { block_number: _ } => None,
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
        Commands::Tx { tx_hash: _ } => None,
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
        Commands::Block { block_number } => block::render(&chain, provider, *block_number).await?,
        Commands::Snapshot => snapshot::render(exchange.unwrap()),
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
        Commands::Trace => {
            trace::render(chain, provider, exchange.unwrap(), cli.num_blocks, cancellation_token)
                .await?
        },
        Commands::Tx { tx_hash } => tx::render(provider, *tx_hash).await?,
    }

    Ok(())
}
