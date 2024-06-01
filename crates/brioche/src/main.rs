use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use brioche_core::reporter::ConsoleReporterKind;
use clap::Parser;

mod build;
mod check;
mod format;
mod install;
mod lsp;
mod publish;
mod run;
mod run_sandbox;

#[derive(Debug, Parser)]
#[command(version)]
enum Args {
    Build(build::BuildArgs),

    Run(run::RunArgs),

    Install(install::InstallArgs),

    Check(check::CheckArgs),

    #[command(name = "fmt")]
    Format(format::FormatArgs),

    Publish(publish::PublishArgs),

    Lsp(lsp::LspArgs),

    Analyze(AnalyzeArgs),

    ExportProject(ExportProjectArgs),

    RunSandbox(run_sandbox::RunSandboxArgs),
}

fn main() -> anyhow::Result<ExitCode> {
    let args = Args::parse();

    match args {
        Args::Build(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(build::build(args))?;

            Ok(exit_code)
        }
        Args::Run(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(run::run(args))?;

            Ok(exit_code)
        }
        Args::Install(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(install::install(args))?;

            Ok(exit_code)
        }
        Args::Check(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(check::check(args))?;

            Ok(exit_code)
        }
        Args::Format(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(format::format(args))?;

            Ok(exit_code)
        }
        Args::Publish(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(publish::publish(args))?;

            Ok(exit_code)
        }
        Args::Lsp(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(lsp::lsp(args))?;

            Ok(ExitCode::SUCCESS)
        }
        Args::Analyze(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(analyze(args))?;

            Ok(ExitCode::SUCCESS)
        }
        Args::ExportProject(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(export_project(args))?;

            Ok(ExitCode::SUCCESS)
        }
        Args::RunSandbox(args) => {
            let exit_code = run_sandbox::run_sandbox(args);

            Ok(exit_code)
        }
    }
}

#[derive(Debug, Parser)]
struct AnalyzeArgs {
    #[arg(short, long)]
    project: PathBuf,
}

async fn analyze(args: AnalyzeArgs) -> anyhow::Result<()> {
    let vfs = brioche_core::vfs::Vfs::immutable();
    let project = brioche_core::project::analyze::analyze_project(&vfs, &args.project).await?;
    println!("{project:#?}");
    Ok(())
}

#[derive(Debug, Parser)]
struct ExportProjectArgs {
    #[arg(short, long)]
    project: PathBuf,
}

async fn export_project(args: ExportProjectArgs) -> anyhow::Result<()> {
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ExportedProject {
        project_hash: brioche_core::project::ProjectHash,
        project: Arc<brioche_core::project::Project>,
        referenced_projects:
            HashMap<brioche_core::project::ProjectHash, Arc<brioche_core::project::Project>>,
    }

    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Plain)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche_core::project::Projects::default();
    let project_hash = projects.load(&brioche, &args.project, true).await?;
    let project = projects.project(project_hash)?;
    let mut project_references = brioche_core::references::ProjectReferences::default();
    brioche_core::references::project_references(
        &brioche,
        &projects,
        &mut project_references,
        [project_hash],
    )
    .await?;

    let exported = ExportedProject {
        project,
        project_hash,
        referenced_projects: project_references.projects.clone(),
    };
    let serialized = serde_json::to_string_pretty(&exported)?;

    guard.shutdown_console().await;

    println!("{}", serialized);
    Ok(())
}

async fn load_project(
    brioche: &brioche_core::Brioche,
    projects: &brioche_core::project::Projects,
    project_arg: Option<&Path>,
    registry_arg: Option<&str>,
) -> anyhow::Result<brioche_core::project::ProjectHash> {
    let project_hash = match (project_arg, registry_arg) {
        (Some(project), None) => projects.load(brioche, project, true).await?,
        (None, Some(registry)) => {
            projects
                .load_from_registry(brioche, registry, &brioche_core::project::Version::Any)
                .await?
        }
        (None, None) => {
            // Default to the current directory if a project path
            // is not specified
            projects.load(brioche, &PathBuf::from("."), true).await?
        }
        (Some(_), Some(_)) => {
            anyhow::bail!("cannot specify both --project and --registry");
        }
    };

    Ok(project_hash)
}
