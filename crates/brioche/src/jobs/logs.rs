use std::path::PathBuf;

use brioche_core::process_events::display::{DisplayEventsOptions, display_events};
use clap::Parser;
use notify::Watcher as _;

use crate::jobs::log_file_reader_from_stdin;

use super::log_file_reader_from_path;

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

    /// Output events as the file grows, until the "exited" event is reached.
    #[clap(long)]
    follow: bool,
}

pub fn logs(args: &LogsArgs) -> anyhow::Result<()> {
    let input = if args.path.to_str() == Some("-") {
        anyhow::ensure!(
            !args.follow,
            "cannot specify --follow when reading from stdin"
        );

        log_file_reader_from_stdin()?
    } else {
        log_file_reader_from_path(&args.path)?
    };

    let mut reader = brioche_core::process_events::reader::ProcessEventReader::new(input)?;

    let mut watcher;
    let follow_events = if args.follow {
        let (tx, rx) = std::sync::mpsc::channel();
        watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let result = match event {
                Ok(_) => Ok(()),
                Err(err) => Err(anyhow::anyhow!(err)),
            };
            let _ = tx.send(result);
        })?;

        watcher.watch(&args.path, notify::RecursiveMode::NonRecursive)?;

        Some(rx)
    } else {
        None
    };

    display_events(
        &mut reader,
        &DisplayEventsOptions {
            limit: args.limit,
            reverse: args.reverse,
            follow_events,
        },
    )?;

    Ok(())
}
