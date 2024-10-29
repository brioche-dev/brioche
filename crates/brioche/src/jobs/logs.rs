use std::path::PathBuf;

use brioche_core::process_events::display::{display_events, DisplayEventsOptions};
use clap::Parser;

#[derive(Debug, Parser)]
pub struct LogsArgs {
    /// The path to the event file to view
    path: PathBuf,

    /// Limit the number of events to show (roughly the number of lines)
    #[clap(long)]
    limit: Option<usize>,

    /// Print events in reverse order
    #[clap(short, long)]
    reverse: bool,
}

pub async fn logs(args: LogsArgs) -> anyhow::Result<()> {
    let input = tokio::fs::File::open(&args.path).await?;
    let input = tokio::io::BufReader::new(input);

    let mut reader = brioche_core::process_events::reader::ProcessEventReader::new(input).await?;

    display_events(
        &mut reader,
        DisplayEventsOptions {
            limit: args.limit,
            reverse: args.reverse,
        },
    )
    .await?;

    Ok(())
}
