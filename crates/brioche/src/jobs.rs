use std::process::ExitCode;

use clap::Subcommand;

mod logs;

#[derive(Debug, Subcommand)]
pub enum JobsSubcommand {
    /// View logs for a job
    Logs(logs::LogsArgs),
}

pub fn jobs(command: JobsSubcommand) -> anyhow::Result<ExitCode> {
    match command {
        JobsSubcommand::Logs(args) => {
            logs::logs(&args)?;

            Ok(ExitCode::SUCCESS)
        }
    }
}
