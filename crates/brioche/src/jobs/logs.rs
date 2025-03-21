use std::path::PathBuf;

use brioche_core::{
    process_events::{
        PROCESS_EVENT_MAGIC,
        display::{DisplayEventsOptions, display_events},
    },
    utils::io::NotSeekable,
};
use clap::Parser;
use notify::Watcher as _;

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

const ZSTD_FRAME_MAGIC: &[u8] = &[0x28, 0xB5, 0x2F, 0xFD];
const ZSTD_SKIPPABLE_FRAME_MAGIC: &[u8] = &[0x2A, 0x4D, 0x18];

trait ReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> ReadSeek for T {}

pub fn logs(args: &LogsArgs) -> anyhow::Result<()> {
    let input: Box<dyn ReadSeek> = if args.path.to_str() == Some("-") {
        anyhow::ensure!(
            !args.follow,
            "cannot specify --follow when reading from stdin"
        );

        let mut stdin = std::io::stdin().lock();
        let format = detect_format(&mut stdin)?;

        match format {
            LogFileFormat::Zstd => {
                let decoder = zstd_framed::ZstdReader::builder_buffered(stdin).build()?;
                Box::new(NotSeekable(decoder))
            }
            LogFileFormat::Bin => Box::new(NotSeekable(stdin)),
        }
    } else {
        let file = std::fs::File::open(&args.path)?;
        let mut buf_reader = std::io::BufReader::new(file);
        let format = detect_format(&mut buf_reader)?;

        match format {
            LogFileFormat::Zstd => {
                let seek_table = zstd_framed::table::read_seek_table(&mut buf_reader)
                    .ok()
                    .flatten();
                let mut decoder = zstd_framed::ZstdReader::builder(buf_reader);
                if let Some(seek_table) = seek_table {
                    decoder = decoder.with_seek_table(seek_table);
                }
                let decoder = decoder.build()?;
                Box::new(decoder)
            }
            LogFileFormat::Bin => Box::new(buf_reader),
        }
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

fn detect_format(reader: &mut impl std::io::BufRead) -> anyhow::Result<LogFileFormat> {
    let buf = reader.fill_buf()?;

    let process_event_magic = PROCESS_EVENT_MAGIC.as_bytes();
    let process_event_magic_partial_len = process_event_magic.len().min(buf.len());
    let process_event_magic_partial = &process_event_magic[0..process_event_magic_partial_len];
    if buf.starts_with(process_event_magic_partial) {
        return Ok(LogFileFormat::Bin);
    }

    let zstd_frame_magic_partial_len = ZSTD_FRAME_MAGIC.len().min(buf.len());
    let zstd_frame_magic_partial = &ZSTD_FRAME_MAGIC[0..zstd_frame_magic_partial_len];
    if buf.starts_with(zstd_frame_magic_partial) {
        return Ok(LogFileFormat::Zstd);
    }

    let zstd_skippable_frame_magic_partial_len = ZSTD_SKIPPABLE_FRAME_MAGIC.len().min(buf.len());
    let zstd_skippable_frame_magic_partial =
        &ZSTD_SKIPPABLE_FRAME_MAGIC[0..zstd_skippable_frame_magic_partial_len];
    if buf.starts_with(zstd_skippable_frame_magic_partial) {
        return Ok(LogFileFormat::Zstd);
    }

    anyhow::bail!("could not detect log file format");
}

#[derive(Debug, Clone, Copy)]
enum LogFileFormat {
    Zstd,
    Bin,
}
