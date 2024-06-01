use std::{path::PathBuf, process::ExitCode};

use brioche_core::reporter::ConsoleReporterKind;
use clap::Parser;
use tracing::Instrument;

#[derive(Debug, Parser)]
pub struct CheckArgs {
    #[arg(short, long)]
    project: Option<PathBuf>,
    #[arg(short, long)]
    registry: Option<String>,
}

pub async fn check(args: CheckArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche_core::project::Projects::default();

    let check_future = async {
        let project_hash = super::load_project(
            &brioche,
            &projects,
            args.project.as_deref(),
            args.registry.as_deref(),
        )
        .await?;

        let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
        if num_lockfiles_updated > 0 {
            tracing::info!(num_lockfiles_updated, "updated lockfiles");
        }

        let checked = brioche_core::script::check::check(&brioche, &projects, project_hash).await?;

        guard.shutdown_console().await;

        let result = checked.ensure_ok(brioche_core::script::check::DiagnosticLevel::Message);

        match result {
            Ok(()) => {
                println!("No errors found 🎉");
                anyhow::Ok(ExitCode::SUCCESS)
            }
            Err(diagnostics) => {
                diagnostics.write(&brioche.vfs, &mut std::io::stdout())?;
                anyhow::Ok(ExitCode::FAILURE)
            }
        }
    };

    let exit_code = check_future
        .instrument(tracing::info_span!("check"))
        .await?;

    Ok(exit_code)
}
