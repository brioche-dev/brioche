use std::{collections::HashMap, path::PathBuf, process::ExitCode, sync::Arc};

use brioche_core::{
    project::{ProjectLocking, ProjectValidation},
    reporter::console::ConsoleReporterKind,
};
use clap::Parser;

mod build;
mod check;
mod format;
mod install;
mod jobs;
mod live_update;
mod lsp;
mod migrate_registry_to_cache;
mod publish;
mod run;
mod run_sandbox;
mod self_update;

#[derive(Debug, Parser)]
#[command(version)]
enum Args {
    /// Build a project
    Build(build::BuildArgs),

    /// Build a project, then run the result
    Run(run::RunArgs),

    /// Build a project, then install it globally
    Install(install::InstallArgs),

    /// Check a project for type errors
    Check(check::CheckArgs),

    /// Format the Brioche files in a project
    #[command(name = "fmt")]
    Format(format::FormatArgs),

    /// Show information about jobs, such as failed builds
    #[command(subcommand)]
    Jobs(jobs::JobsSubcommand),

    /// Publish a project to a registry
    Publish(publish::PublishArgs),

    /// Update a Brioche project based on the current version of
    /// the upstream project
    LiveUpdate(live_update::LiveUpdateArgs),

    /// Start the Language Server Protocol server
    Lsp(lsp::LspArgs),

    /// Update Brioche itself
    SelfUpdate(self_update::SelfUpdateArgs),

    /// Internal tool: analyze a project
    #[command(hide = true)]
    Analyze(AnalyzeArgs),

    /// Internal tool: serialize an entire project as JSON
    #[command(hide = true)]
    ExportProject(ExportProjectArgs),

    /// Internal tool: migrate bakes/artifacts/projects from a registry to a cache
    #[command(hide = true)]
    MigrateRegistryToCache(migrate_registry_to_cache::MigrateRegistryToCacheArgs),

    /// Used by Brioche itself to run a sandboxed process
    #[command(hide = true)]
    RunSandbox(run_sandbox::RunSandboxArgs),
}

fn main() -> anyhow::Result<ExitCode> {
    let args = Args::parse();

    match args {
        Args::Build(args) => {
            let js_platform = brioche_core::script::initialize_js_platform();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(build::build(js_platform, args))?;

            Ok(exit_code)
        }
        Args::Run(args) => {
            let js_platform = brioche_core::script::initialize_js_platform();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(run::run(js_platform, args))?;

            Ok(exit_code)
        }
        Args::Install(args) => {
            let js_platform = brioche_core::script::initialize_js_platform();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(install::install(js_platform, args))?;

            Ok(exit_code)
        }
        Args::Check(args) => {
            let js_platform = brioche_core::script::initialize_js_platform();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(check::check(js_platform, args))?;

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
            let js_platform = brioche_core::script::initialize_js_platform();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(publish::publish(js_platform, args))?;

            Ok(exit_code)
        }
        Args::LiveUpdate(args) => {
            let js_platform = brioche_core::script::initialize_js_platform();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(live_update::live_update(js_platform, args))?;

            Ok(ExitCode::SUCCESS)
        }
        Args::Lsp(args) => {
            let js_platform = brioche_core::script::initialize_js_platform();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(lsp::lsp(js_platform, args))?;

            Ok(ExitCode::SUCCESS)
        }
        Args::SelfUpdate(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let updated = rt.block_on(self_update::self_update(args))?;

            if updated {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::FAILURE)
            }
        }
        Args::Jobs(command) => jobs::jobs(command),
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
        Args::MigrateRegistryToCache(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(migrate_registry_to_cache::migrate_registry_to_cache(args))?;

            Ok(ExitCode::SUCCESS)
        }
        Args::RunSandbox(args) => {
            let exit_code = run_sandbox::run_sandbox(&args);

            Ok(exit_code)
        }
    }
}

#[derive(Debug, Parser)]
struct AnalyzeArgs {
    #[arg(short, long)]
    project: PathBuf,
}

#[expect(clippy::print_stdout)]
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

#[expect(clippy::print_stdout)]
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
        brioche_core::reporter::console::start_console_reporter(ConsoleReporterKind::Plain)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche_core::project::Projects::default();
    let project_hash = projects
        .load(
            &brioche,
            &args.project,
            ProjectValidation::Standard,
            ProjectLocking::Unlocked,
        )
        .await?;
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

    println!("{serialized}");
    Ok(())
}

#[derive(Debug, clap::Args)]
struct ProjectArgs {
    /// The path of the project directory to build [default: .]
    #[clap(short, long)]
    project: Option<PathBuf>,

    /// The name of a registry project to build
    #[clap(short, long)]
    registry: Option<String>,
}

#[derive(Debug, clap::Args)]
#[group(required = false, multiple = false)]
struct MultipleProjectArgs {
    /// The path of the project directory to build [default: .]
    #[clap(short, long)]
    project: Vec<PathBuf>,

    /// The name of a registry project to build
    #[clap(id = "registry", short, long)]
    registry_project: Vec<String>,
}

async fn load_project(
    brioche: &brioche_core::Brioche,
    projects: &brioche_core::project::Projects,
    args: &ProjectArgs,
    locking: ProjectLocking,
) -> anyhow::Result<brioche_core::project::ProjectHash> {
    let project_hash = match (&args.project, &args.registry) {
        (Some(project), None) => {
            projects
                .load(brioche, project, ProjectValidation::Standard, locking)
                .await?
        }
        (None, Some(registry)) => {
            projects
                .load_from_registry(brioche, registry, &brioche_core::project::Version::Any)
                .await?
        }
        (None, None) => {
            // Default to the current directory if a project path
            // is not specified
            projects
                .load(
                    brioche,
                    &PathBuf::from("."),
                    ProjectValidation::Standard,
                    ProjectLocking::Unlocked,
                )
                .await?
        }
        (Some(_), Some(_)) => {
            anyhow::bail!("cannot specify both --project and --registry");
        }
    };

    Ok(project_hash)
}

fn consolidate_result(
    reporter: &brioche_core::reporter::Reporter,
    project_name: &String,
    result: Result<bool, anyhow::Error>,
    error_result: &mut Option<()>,
) {
    match result {
        Err(err) => {
            reporter.emit(superconsole::Lines::from_multiline_string(
                &format!("Error occurred with {project_name}: {err}"),
                superconsole::style::ContentStyle {
                    foreground_color: Some(superconsole::style::Color::Red),
                    ..superconsole::style::ContentStyle::default()
                },
            ));

            *error_result = Some(());
        }
        Ok(false) => {
            *error_result = Some(());
        }
        _ => {}
    }
}

#[derive(Debug, Default, Clone, Copy, clap::ValueEnum)]
enum DisplayMode {
    /// Display with console output if stdout is a tty, otherwise use
    /// plain output.
    #[default]
    Auto,

    /// Pretty console-based output.
    Console,

    /// Plaintext output.
    Plain,

    /// Plaintext output with less stuff, e.g. by hiding process outputs.
    PlainReduced,
}

impl DisplayMode {
    const fn to_console_reporter_kind(
        self,
    ) -> brioche_core::reporter::console::ConsoleReporterKind {
        match self {
            Self::Auto => brioche_core::reporter::console::ConsoleReporterKind::Auto,
            Self::Console => brioche_core::reporter::console::ConsoleReporterKind::SuperConsole,
            Self::Plain => brioche_core::reporter::console::ConsoleReporterKind::Plain,
            Self::PlainReduced => {
                brioche_core::reporter::console::ConsoleReporterKind::PlainReduced
            }
        }
    }
}

/// Start a task that handles Ctrl-C. When Ctrl-C is received, all critical
/// tasks will be cancelled, and the program will exit once they have all
/// exited.
pub fn start_shutdown_handler(brioche: brioche_core::Brioche) {
    tokio::task::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        brioche.cancel_tasks();
        brioche.wait_for_tasks().await;
        std::process::exit(1);
    });
}
