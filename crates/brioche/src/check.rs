use std::path::PathBuf;
use std::process::ExitCode;
use std::vec;

use brioche_core::project::ProjectHash;
use brioche_core::project::Projects;
use brioche_core::reporter::ConsoleReporterKind;
use brioche_core::Brioche;
use clap::Parser;
use tracing::Instrument;

use crate::consolidate_result;

#[derive(Debug, Parser)]
pub struct CheckArgs {
    #[command(flatten)]
    project: super::MultipleProjectArgs,
}

pub async fn check(args: CheckArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    let projects = brioche_core::project::Projects::default();

    let mut error_result = Option::None;

    // Handle the case where no projects and no registries are specified
    let projects_path = if args.project.project.is_empty() && args.project.registry.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.project.project
    };

    // Loop over the projects
    for project_path in projects_path {
        let project_name = format!("project '{name}'", name = project_path.display());

        match projects.load(&brioche, &project_path, true).await {
            Ok(project_hash) => {
                let result = run_check(&brioche, &projects, project_hash, &project_name).await;
                consolidate_result(&project_name, result, &mut error_result);
            }
            Err(e) => {
                consolidate_result(&project_name, Err(e), &mut error_result);
            }
        }
    }

    // Loop over the registries
    for registry in args.project.registry {
        let registry_name = format!("registry '{registry}'");

        match projects
            .load_from_registry(&brioche, &registry, &brioche_core::project::Version::Any)
            .await
        {
            Ok(project_hash) => {
                let result = run_check(&brioche, &projects, project_hash, &registry_name).await;
                consolidate_result(&registry_name, result, &mut error_result);
            }
            Err(e) => {
                consolidate_result(&registry_name, Err(e), &mut error_result);
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

async fn run_check(
    brioche: &Brioche,
    projects: &Projects,
    project_hash: ProjectHash,
    project_name: &String,
) -> Result<bool, anyhow::Error> {
    let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
    if num_lockfiles_updated > 0 {
        tracing::info!(num_lockfiles_updated, "updated lockfiles");
    }

    let result =
        async { brioche_core::script::check::check(brioche, projects, project_hash).await }
            .instrument(tracing::info_span!("check"))
            .await?
            .ensure_ok(brioche_core::script::check::DiagnosticLevel::Message);

    match result {
        Ok(()) => {
            println!("No errors found on {project_name} 🎉");
            Ok(true)
        }
        Err(diagnostics) => {
            diagnostics.write(&brioche.vfs, &mut std::io::stdout())?;
            Ok(false)
        }
    }
}
