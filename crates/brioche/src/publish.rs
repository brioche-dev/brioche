use std::{path::PathBuf, process::ExitCode};

use brioche_core::reporter::ConsoleReporterKind;
use clap::Parser;

#[derive(Debug, Parser)]
pub struct PublishArgs {
    /// The path to the project directory to publish
    #[arg(short, long)]
    project: PathBuf,
}

pub async fn publish(args: PublishArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche_core::project::Projects::default();
    let project_hash = projects.load(&brioche, &args.project, true).await?;

    let project = projects.project(project_hash)?;
    let name = project.definition.name.as_deref().unwrap_or("[unnamed]");
    let version = project
        .definition
        .version
        .as_deref()
        .unwrap_or("[unversioned]");

    let lockfile_result = projects.validate_no_dirty_lockfiles();
    match lockfile_result {
        Ok(()) => {}
        Err(error) => {
            eprintln!("{error:#}");
            return Ok(ExitCode::FAILURE);
        }
    }

    let checked = brioche_core::script::check::check(&brioche, &projects, project_hash).await?;
    let check_results = checked.ensure_ok(brioche_core::script::check::DiagnosticLevel::Warning);
    match check_results {
        Ok(()) => {
            println!("No errors found 🎉");
        }
        Err(diagnostics) => {
            diagnostics.write(&brioche.vfs, &mut std::io::stdout())?;
            return Ok(ExitCode::FAILURE);
        }
    }

    guard.shutdown_console().await;

    let response =
        brioche_core::publish::publish_project(&brioche, &projects, project_hash, true).await?;

    if response.tags.is_empty() {
        println!("Project already up to date: {} {}", name, version);
    } else {
        println!("🚀 Published project {} {}", name, version);
        println!();
        println!("Updated tags:");
        for tag in &response.tags {
            let status = if tag.previous_hash.is_some() {
                "updated"
            } else {
                "new"
            };
            println!("  {} {} ({status})", tag.name, tag.tag);
        }
    }

    Ok(ExitCode::SUCCESS)
}
