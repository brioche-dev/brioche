use std::{path::PathBuf, process::ExitCode};

use brioche_core::reporter::ConsoleReporterKind;
use clap::Parser;
use tracing::Instrument;

#[derive(Debug, Parser)]
pub struct FormatArgs {
    /// The path to the project directory to format
    #[arg(short, long)]
    project: PathBuf,

    /// Check formatting without writing changes
    #[arg(long)]
    check: bool,
}

pub async fn format(args: FormatArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche_core::project::Projects::default();

    let format_future = async {
        let project_hash = projects.load(&brioche, &args.project, true).await?;

        if args.check {
            let mut unformatted_files =
                brioche_core::script::format::check_format(&projects, project_hash).await?;
            unformatted_files.sort();

            guard.shutdown_console().await;

            if unformatted_files.is_empty() {
                println!("All files formatted");
                Ok(ExitCode::SUCCESS)
            } else {
                println!("The following files are not formatted:");
                for file in unformatted_files {
                    println!("- {}", file.display());
                }

                Ok(ExitCode::FAILURE)
            }
        } else {
            brioche_core::script::format::format(&projects, project_hash).await?;

            guard.shutdown_console().await;

            anyhow::Ok(ExitCode::SUCCESS)
        }
    };

    let exit_code = format_future
        .instrument(tracing::info_span!("format"))
        .await?;

    Ok(exit_code)
}
