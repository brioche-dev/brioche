use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use brioche_core::reporter::{ConsoleReporterKind, Reporter};
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

    let mut error_result = Option::None;
    for project_path in args.project {
        match project_format(&reporter, &project_path, args.check).await {
            Err(err) => {
                println!(
                    "Error occurred while formatting project '{project_path}': {err}",
                    project_path = project_path.display(),
                    err = err
                );

                error_result = Some(());
            }
            Ok(false) => {
                error_result = Some(());
            }
            _ => {}
        }
    }

    guard.shutdown_console().await;

    let exit_code = if error_result.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };

    Ok(exit_code)
}

async fn project_format(
    reporter: &Reporter,
    project_path: &Path,
    check: bool,
) -> Result<bool, anyhow::Error> {
    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    let projects = brioche_core::project::Projects::default();

    let project_hash = projects.load(&brioche, project_path, true).await?;

    let result = async {
        if check {
            brioche_core::script::format::check_format(&projects, project_hash).await
        } else {
            brioche_core::script::format::format(&projects, project_hash).await
        }
    }
    .instrument(tracing::info_span!("format"))
    .await;

    match result {
        Err(err) => Err(err),
        Ok(mut files) => {
            files.sort();

            if !check {
                if !files.is_empty() {
                    println!(
                        "The following files of project '{project_path}' have been formatted:\n{files}",
                        project_path = project_path.display(),
                        files = files
                            .iter()
                            .map(|file| format!("- {}", file.display()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                }

                Ok(true)
            } else if files.is_empty() {
                println!(
                    "All files of project '{project_path}' are formatted",
                    project_path = project_path.display()
                );

                Ok(true)
            } else {
                println!(
                    "The following files of project '{project_path}' are not formatted:\n{files}",
                    project_path = project_path.display(),
                    files = files
                        .iter()
                        .map(|file| format!("- {}", file.display()))
                        .collect::<Vec<_>>()
                        .join("\n")
                );

                Ok(false)
            }
        }
    }
}
