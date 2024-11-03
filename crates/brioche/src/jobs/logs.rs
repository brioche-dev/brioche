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

pub fn logs(args: LogsArgs) -> anyhow::Result<()> {
    let input = brioche_core::utils::zstd::ZstdSmartDecoder::create(|| {
        let input = std::fs::File::open(&args.path)?;
        anyhow::Ok(std::io::BufReader::new(input))
    })?;

    let mut reader = brioche_core::process_events::reader::ProcessEventReader::new(input)?;

    display_events(
        &mut reader,
        DisplayEventsOptions {
            limit: args.limit,
            reverse: args.reverse,
        },
    )?;

    Ok(())
}
