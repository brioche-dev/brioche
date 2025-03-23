use std::{path::Path, process::ExitCode};

use brioche_core::{process_events::PROCESS_EVENT_MAGIC, utils::io::NotSeekable};
use clap::Subcommand;

mod debug_shell;
mod logs;

#[derive(Debug, Subcommand)]
pub enum JobsSubcommand {
    /// View logs for a job
    Logs(logs::LogsArgs),

    /// Start a shell in an exited job for debugging.
    ///
    /// Note that the job's event file is used for determining which host
    /// paths should be included in the sandbox, so only use this command
    /// for event files from sources you trust!
    DebugShell(debug_shell::DebugShellArgs),
}

pub fn jobs(command: JobsSubcommand) -> anyhow::Result<ExitCode> {
    match command {
        JobsSubcommand::Logs(args) => {
            logs::logs(&args)?;

            Ok(ExitCode::SUCCESS)
        }
        JobsSubcommand::DebugShell(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(debug_shell::debug_shell(&args))?;

            Ok(ExitCode::SUCCESS)
        }
    }
}

const ZSTD_FRAME_MAGIC: &[u8] = &[0x28, 0xB5, 0x2F, 0xFD];
const ZSTD_SKIPPABLE_FRAME_MAGIC: &[u8] = &[0x2A, 0x4D, 0x18];

trait ReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> ReadSeek for T {}

fn log_file_reader_from_stdin() -> anyhow::Result<Box<dyn ReadSeek>> {
    let mut stdin = std::io::stdin().lock();
    let format = detect_log_file_format(&mut stdin)?;

    match format {
        LogFileFormat::Zstd => {
            let decoder = zstd_framed::ZstdReader::builder_buffered(stdin).build()?;
            Ok(Box::new(NotSeekable(decoder)))
        }
        LogFileFormat::Bin => Ok(Box::new(NotSeekable(stdin))),
    }
}

fn log_file_reader_from_path(path: &Path) -> anyhow::Result<Box<dyn ReadSeek>> {
    let file = std::fs::File::open(path)?;
    let mut buf_reader = std::io::BufReader::new(file);
    let format = detect_log_file_format(&mut buf_reader)?;

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
            Ok(Box::new(decoder))
        }
        LogFileFormat::Bin => Ok(Box::new(buf_reader)),
    }
}

fn detect_log_file_format(reader: &mut impl std::io::BufRead) -> anyhow::Result<LogFileFormat> {
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
