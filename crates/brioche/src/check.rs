use std::collections::HashMap;
use std::collections::HashSet;
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
    let mut error_result = None;

    // Handle the case where no projects and no registries are specified
    let project_paths =
        if args.project.project.is_empty() && args.project.registry_project.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            args.project.project
        };

    // Pre-allocate capacity for project names and projects to check
    let mut project_names = HashMap::with_capacity(project_paths.len());
    let mut projects_to_check = HashSet::with_capacity(project_paths.len());

    // Load each path project
    for project_path in project_paths {
        let project_name = format!("project '{name}'", name = project_path.display());
        let project_hash = projects
            .load(
                &brioche,
                &project_path,
                ProjectValidation::Standard,
                locking,
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
        projects_to_check.insert(project_hash);
    }

    // Load each registry project
    for registry_project in args.project.registry_project {
        let project_name = format!("registry project '{registry_project}'");
        let project_hash = projects
            .load_from_registry(
                &brioche,
                &registry_project,
                &brioche_core::project::Version::Any,
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
        projects_to_check.insert(project_hash);
    }

    let project_name = if project_names.len() == 1 {
        Some(project_names.values().next().unwrap())
    } else {
        None
    };
    let project_name = project_name.map(String::as_str);

    let result = run_check(
        &reporter,
        &brioche,
        js_platform,
        &projects,
        &projects_to_check,
        project_name,
        &check_options,
    )
    .await;
    consolidate_result(&reporter, project_name, result, &mut error_result);

    guard.shutdown_console().await;
    brioche.wait_for_tasks().await;

    let exit_code = error_result.map_or(ExitCode::SUCCESS, |()| ExitCode::FAILURE);

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
    project_hashes: &HashSet<ProjectHash>,
    project_name: Option<&str>,
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

        brioche_core::script::check::check(brioche, js_platform, projects, project_hashes).await
    }
    .instrument(tracing::info_span!("check"))
    .await?
    .ensure_ok(brioche_core::script::check::DiagnosticLevel::Message);

    match result {
        Ok(()) => {
            let format_string = project_name.map_or_else(
                || "No errors found in projects 🎉".to_string(),
                |project_name| format!("No errors found in {project_name} 🎉"),
            );

            reporter.emit(superconsole::Lines::from_multiline_string(
                &format_string,
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
