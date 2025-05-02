use std::{path::PathBuf, process::ExitCode};

use brioche_core::{
    Brioche,
    project::{ProjectHash, ProjectLocking, ProjectValidation, Projects},
    reporter::Reporter,
};
use clap::Parser;

use crate::consolidate_result;

#[derive(Debug, Parser)]
pub struct PublishArgs {
    /// The path to the project directory to publish
    #[arg(short, long)]
    project: Vec<PathBuf>,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

pub async fn publish(
    js_platform: brioche_core::script::JsPlatform,
    args: PublishArgs,
) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = brioche_core::reporter::console::start_console_reporter(
        args.display.to_console_reporter_kind(),
    )?;

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

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
                ProjectLocking::Locked,
            )
            .await
        {
            Ok(project_hash) => {
                let result = run_publish(
                    &reporter,
                    &brioche,
                    js_platform,
                    &projects,
                    project_hash,
                    &project_name,
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
    brioche.wait_for_tasks().await;

    let exit_code = if error_result.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };

    Ok(exit_code)
}

async fn run_publish(
    reporter: &Reporter,
    brioche: &Brioche,
    js_platform: brioche_core::script::JsPlatform,
    projects: &Projects,
    project_hash: ProjectHash,
    project_name: &String,
) -> Result<bool, anyhow::Error> {
    let project = projects.project(project_hash)?;
    let name = project.definition.name.as_deref().unwrap_or("[unnamed]");
    let version = project
        .definition
        .version
        .as_deref()
        .unwrap_or("[unversioned]");

    projects.validate_no_dirty_lockfiles()?;

    let result = brioche_core::script::check::check(brioche, js_platform, projects, project_hash)
        .await?
        .ensure_ok(brioche_core::script::check::DiagnosticLevel::Warning);

    match result {
        Ok(()) => {
            reporter.emit(superconsole::Lines::from_multiline_string(
                &format!("No errors found in {project_name} 🎉",),
                superconsole::style::ContentStyle::default(),
            ));

            let response =
                brioche_core::publish::publish_project(brioche, projects, project_hash).await?;

            if response.tags.is_empty() {
                reporter.emit(superconsole::Lines::from_multiline_string(
                    &format!("Project already up to date: {name} {version}"),
                    superconsole::style::ContentStyle::default(),
                ));
            } else {
                let tags_info = response
                    .tags
                    .iter()
                    .map(|tag| {
                        let status = if tag.previous_hash.is_some() {
                            "updated"
                        } else {
                            "new"
                        };
                        format!("{} {} ({})", tag.name, tag.tag, status)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                reporter.emit(superconsole::Lines::from_multiline_string(
                    &format!("🚀 Published project {name} {version}\nUpdated tags:\n{tags_info}"),
                    superconsole::style::ContentStyle::default(),
                ));
            }

            Ok(true)
        }
        Err(diagnostics) => {
            let mut output = Vec::new();
            diagnostics.write(&brioche.vfs, &mut output)?;

            reporter.emit(superconsole::Lines::from_multiline_string(
                &String::from_utf8(output)?,
                superconsole::style::ContentStyle::default(),
            ));

            Ok(false)
        }
    }
}
