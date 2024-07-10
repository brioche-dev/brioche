use std::{path::PathBuf, process::ExitCode};

use brioche_core::reporter::ConsoleReporterKind;
use clap::Parser;
use tracing::Instrument;

#[derive(Debug, Parser)]
pub struct FormatArgs {
    /// The path to the project directory to format
    #[arg(short, long)]
    project: Vec<PathBuf>,

    /// Check formatting without writing changes
    #[arg(long)]
    check: bool,
}

pub async fn format(args: FormatArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter).build().await?;

    let format_futures = args
        .project
        .into_iter()
        .map(|project_path| {
            let projects = brioche_core::project::Projects::default();

            async { project_format(projects, brioche.clone(), project_path, args.check).await }
        })
        .map(|project_path| project_path.instrument(tracing::info_span!("format")))
        .collect::<Vec<_>>();

    let mut unformatted_files = Vec::new();
    for future in format_futures {
        unformatted_files.append(&mut future.await?);
    }
    unformatted_files.sort();

    let exit_code = if !args.check {
        ExitCode::SUCCESS
    } else if unformatted_files.is_empty() {
        println!("All files formatted");
        ExitCode::SUCCESS
    } else {
        println!("The following files are not formatted:");
        for file in unformatted_files {
            println!("- {}", file.display());
        }

        ExitCode::FAILURE
    };

    guard.shutdown_console().await;

    Ok(exit_code)
}

async fn project_format(
    projects: brioche_core::project::Projects,
    brioche: brioche_core::Brioche,
    project_path: PathBuf,
    check: bool,
) -> Result<Vec<PathBuf>, anyhow::Error> {
    let project_hash = projects.load(&brioche, &project_path, true).await?;

    if check {
        Ok(brioche_core::script::format::check_format(&projects, project_hash).await?)
    } else {
        brioche_core::script::format::format(&projects, project_hash).await?;

        Ok(vec![])
    }
}
