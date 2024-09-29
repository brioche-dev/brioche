use std::{path::PathBuf, process::ExitCode};

use brioche_core::{
    project::{ProjectHash, ProjectLocking, ProjectValidation, Projects},
    reporter::{ConsoleReporterKind, Reporter},
};
use clap::Parser;
use tracing::Instrument;

use crate::consolidate_result;

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

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    let projects = brioche_core::project::Projects::default();

    let mut error_result = Option::None;

    // Loop over the projects
    for project_path in args.project {
        let project_name = format!("project '{name}'", name = project_path.display());

        match projects
            .load(
                &brioche,
                &project_path,
                ProjectValidation::Standard,
                ProjectLocking::Unlocked,
            )
            .await
        {
            Ok(project_hash) => {
                let result = run_format(
                    &reporter,
                    &projects,
                    project_hash,
                    &project_name,
                    args.check,
                )
                .await;
                consolidate_result(&reporter, &project_name, result, &mut error_result);
            }
            Err(e) => {
                consolidate_result(&reporter, &project_name, Err(e), &mut error_result);
            }
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

async fn run_format(
    reporter: &Reporter,
    projects: &Projects,
    project_hash: ProjectHash,
    project_name: &String,
    check: bool,
) -> Result<bool, anyhow::Error> {
    let result = async {
        if check {
            brioche_core::script::format::check_format(projects, project_hash).await
        } else {
            brioche_core::script::format::format(projects, project_hash).await
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
                    reporter.emit(superconsole::Lines::from_multiline_string(
                        &format!(
                            "The following files of {project_name} have been formatted:\n{files}",
                            files = files
                                .iter()
                                .map(|file| format!("- {}", file.display()))
                                .collect::<Vec<_>>()
                                .join("\n")
                        ),
                        superconsole::style::ContentStyle::default(),
                    ));
                }

                Ok(true)
            } else if files.is_empty() {
                reporter.emit(superconsole::Lines::from_multiline_string(
                    &format!("All files of {project_name} are formatted",),
                    superconsole::style::ContentStyle::default(),
                ));

                Ok(true)
            } else {
                reporter.emit(superconsole::Lines::from_multiline_string(
                    &format!(
                        "The following files of {project_name} are not formatted:\n{files}",
                        files = files
                            .iter()
                            .map(|file| format!("- {}", file.display()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                    superconsole::style::ContentStyle::default(),
                ));

                Ok(false)
            }
        }
    }
}
