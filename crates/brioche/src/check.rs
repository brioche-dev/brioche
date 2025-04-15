use std::path::PathBuf;
use std::process::ExitCode;

use brioche_core::Brioche;
use brioche_core::project::ProjectHash;
use brioche_core::project::ProjectLocking;
use brioche_core::project::ProjectValidation;
use brioche_core::project::Projects;
use brioche_core::reporter::Reporter;
use clap::Parser;
use tracing::Instrument as _;

use crate::consolidate_result;

#[derive(Debug, Parser)]
pub struct CheckArgs {
    /// Validate that the lockfile is up-to-date
    #[arg(long)]
    locked: bool,

    #[command(flatten)]
    project: super::MultipleProjectArgs,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

pub async fn check(
    js_platform: brioche_core::script::JsPlatform,
    args: CheckArgs,
) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = brioche_core::reporter::console::start_console_reporter(
        args.display.to_console_reporter_kind(),
    )?;

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

    let projects = brioche_core::project::Projects::default();

    let check_options = CheckOptions {
        locked: args.locked,
    };
    let locking = if args.locked {
        ProjectLocking::Locked
    } else {
        ProjectLocking::Unlocked
    };
    let mut error_result = Option::None;

    // Handle the case where no projects and no registries are specified
    let projects_path =
        if args.project.project.is_empty() && args.project.registry_project.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            args.project.project
        };

    // Loop over the projects
    for project_path in projects_path {
        let project_name = format!("project '{name}'", name = project_path.display());

        match projects
            .load(
                &brioche,
                &project_path,
                ProjectValidation::Standard,
                locking,
            )
            .await
        {
            Ok(project_hash) => {
                let result = run_check(
                    &reporter,
                    &brioche,
                    js_platform,
                    &projects,
                    project_hash,
                    &project_name,
                    &check_options,
                )
                .await;
                consolidate_result(&reporter, &project_name, result, &mut error_result);
            }
            Err(e) => {
                consolidate_result(&reporter, &project_name, Err(e), &mut error_result);
            }
        }
    }

    // Loop over the registry projects
    for registry_project in args.project.registry_project {
        let project_name = format!("registry project '{registry_project}'");

        match projects
            .load_from_registry(
                &brioche,
                &registry_project,
                &brioche_core::project::Version::Any,
            )
            .await
        {
            Ok(project_hash) => {
                let result = run_check(
                    &reporter,
                    &brioche,
                    js_platform,
                    &projects,
                    project_hash,
                    &project_name,
                    &check_options,
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

struct CheckOptions {
    locked: bool,
}

async fn run_check(
    reporter: &Reporter,
    brioche: &Brioche,
    js_platform: brioche_core::script::JsPlatform,
    projects: &Projects,
    project_hash: ProjectHash,
    project_name: &String,
    options: &CheckOptions,
) -> Result<bool, anyhow::Error> {
    let result = async {
        // If the `--locked` flag is used, validate that all lockfiles are
        // up-to-date. Otherwise, write any out-of-date lockfiles
        if options.locked {
            projects.validate_no_dirty_lockfiles()?;
        } else {
            let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
            if num_lockfiles_updated > 0 {
                tracing::info!(num_lockfiles_updated, "updated lockfiles");
            }
        }

        brioche_core::script::check::check(brioche, js_platform, projects, project_hash).await
    }
    .instrument(tracing::info_span!("check"))
    .await?
    .ensure_ok(brioche_core::script::check::DiagnosticLevel::Message);

    match result {
        Ok(()) => {
            reporter.emit(superconsole::Lines::from_multiline_string(
                &format!("No errors found in {project_name} 🎉",),
                superconsole::style::ContentStyle::default(),
            ));

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
