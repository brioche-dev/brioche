use std::process::ExitCode;

use clap::Subcommand;

mod logs;

#[derive(Debug, Subcommand)]
pub enum JobsSubcommand {
    Logs(logs::LogsArgs),
}

pub fn jobs(command: JobsSubcommand) -> anyhow::Result<ExitCode> {
    match command {
        JobsSubcommand::Logs(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(logs::logs(args))?;

            Ok(ExitCode::SUCCESS)
        }
    }
}
