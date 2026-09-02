use std::{io::Write, pin::pin};

use alloy::providers::Provider;
use crossterm::{
    QueueableCommand,
    cursor::MoveTo,
    execute,
    style::Print,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use perpl_sdk::{Chain, state::Exchange, stream};
use tokio_util::sync::CancellationToken;

use crate::{args::BookArgs, highlight::Highlights};

pub(crate) async fn render<P: Provider + Clone>(
    chain: Chain,
    provider: P,
    mut exchange: Exchange,
    book_args: &BookArgs,
    highlights: &Highlights,
    num_blocks: Option<u64>,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let stream = stream::raw(&chain, provider, exchange.instant().next(), tokio::time::sleep);
    let mut stream = pin!(stream);

    let mut blocks_left = num_blocks;

    let mut stdout = std::io::stdout();

    execute!(stdout, EnterAlternateScreen, Clear(ClearType::All), MoveTo(0, 0))?;

    while let Some(res) = stream.next().await {
        if cancellation_token.is_cancelled() || blocks_left.is_some_and(|count| count == 0) {
            break;
        }

        let block_events = res?;
        exchange.apply_events(&block_events)?;

        let perpetual = exchange.perpetuals().values().last().unwrap();

        stdout.queue(Clear(ClearType::All))?;
        stdout.queue(MoveTo(0, 0))?;

        // Exchange summary
        stdout.queue(Print(format!("{}", exchange)))?;

        // Perpetual summary
        stdout.queue(Print(format!("{}", perpetual)))?;

        // Book
        let book_view = perpetual
            .l3_book()
            .view(book_args.depth(), book_args.orders_per_level(), book_args.show_expired)
            .highlighted_by(highlights);
        stdout.queue(Print(format!("{:#}", book_view)))?;

        stdout.flush()?;

        if let Some(ref mut count) = blocks_left {
            *count -= 1;
        }
    }

    execute!(stdout, LeaveAlternateScreen)?;

    Ok(())
}
