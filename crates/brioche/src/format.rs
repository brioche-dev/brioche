use std::collections::HashMap;
use std::collections::HashSet;
use std::{path::PathBuf, process::ExitCode};

use brioche_core::{
    project::{ProjectHash, ProjectLocking, ProjectValidation, Projects},
    reporter::Reporter,
};
use clap::Parser;
use tracing::Instrument as _;

use crate::consolidate_result;

#[derive(Debug, Parser)]
pub struct FormatArgs {
    /// The path to the project directory to format
    #[arg(short, long, default_value = ".")]
    project: Vec<PathBuf>,

    /// Check formatting without writing changes
    #[arg(long)]
    check: bool,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

pub async fn format(args: FormatArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = brioche_core::reporter::console::start_console_reporter(
        args.display.to_console_reporter_kind(),
    )?;

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

    let projects = brioche_core::project::Projects::default();

    let mut error_result = Option::None;

    // Pre-allocate capacity for project names and projects to format
    let mut project_names = HashMap::with_capacity(args.project.len());
    let mut projects_to_format = HashSet::with_capacity(args.project.len());

    // Load each path project
    for project_path in args.project {
        let project_name = format!("project '{name}'", name = project_path.display());
        let project_hash = projects
            .load(
                &brioche,
                &project_path,
                ProjectValidation::Standard,
                ProjectLocking::Unlocked,
            )
            .await;

        let project_hash = match project_hash {
            Ok(project_hash) => project_hash,
            Err(error) => {
                consolidate_result(
                    &reporter,
                    Some(&project_name),
                    Err(error),
                    &mut error_result,
                );
                continue;
            }
        };

        project_names.entry(project_hash).or_insert(project_name);
        projects_to_format.insert(project_hash);
    }

    let project_name = if project_names.len() == 1 {
        Some(project_names.values().next().unwrap())
    } else {
        None
    };
    let project_name = project_name.map(String::as_str);

    let result = run_format(
        &reporter,
        &projects,
        &projects_to_format,
        project_name,
        args.check,
    )
    .await;
    consolidate_result(&reporter, project_name, result, &mut error_result);

    guard.shutdown_console().await;
    brioche.wait_for_tasks().await;

    let exit_code = error_result.map_or(ExitCode::SUCCESS, |()| ExitCode::FAILURE);

    Ok(exit_code)
}

async fn run_format(
    reporter: &Reporter,
    projects: &Projects,
    project_hashes: &HashSet<ProjectHash>,
    project_name: Option<&str>,
    check: bool,
) -> Result<bool, anyhow::Error> {
    let result = async {
        if check {
            brioche_core::script::format::check_format(projects, project_hashes).await
        } else {
            brioche_core::script::format::format(projects, project_hashes).await
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
                    let files = files
                        .iter()
                        .map(|file| format!("- {}", file.display()))
                        .collect::<Vec<_>>()
                        .join("\n");

                    let format_string = project_name.map_or_else(|| format!("The following files in projects have been formatted:\n{files}"), |project_name| format!(
                            "The following files of {project_name} have been formatted:\n{files}"
                        ));

                    reporter.emit(superconsole::Lines::from_multiline_string(
                        &format_string,
                        superconsole::style::ContentStyle::default(),
                    ));
                }

                Ok(true)
            } else if files.is_empty() {
                let format_string = project_name.map_or_else(
                    || "All files in projects are formatted".to_string(),
                    |project_name| format!("All files of {project_name} are formatted"),
                );

                reporter.emit(superconsole::Lines::from_multiline_string(
                    &format_string,
                    superconsole::style::ContentStyle::default(),
                ));

                Ok(true)
            } else {
                let files = files
                    .iter()
                    .map(|file| format!("- {}", file.display()))
                    .collect::<Vec<_>>()
                    .join("\n");

                let format_string = project_name.map_or_else(
                    || format!("The following files in projects are not formatted:\n{files}"),
                    |project_name| {
                        format!("The following files of {project_name} are not formatted:\n{files}")
                    },
                );

                reporter.emit(superconsole::Lines::from_multiline_string(
                    &format_string,
                    superconsole::style::ContentStyle::default(),
                ));

                Ok(false)
            }
        }
    }
}
