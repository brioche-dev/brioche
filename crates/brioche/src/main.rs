use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::utils::{ProjectRefs, ProjectRefsParser};

mod build;
mod run_sandbox;
mod utils;
mod weird_ui;

#[derive(Debug, Parser)]
#[command(version)]
enum Args {
    /// Build a project
    Build(build::BuildArgs),

    /// Start a Weird UI to interactively explore and debug a project
    #[cfg_attr(not(feature = "weird-ui"), command(hide = true))]
    WeirdUi(WeirdUiArgs),

    /// Used by Brioche itself to run a sandboxed process
    #[command(hide = true)]
    RunSandbox(run_sandbox::RunSandboxArgs),
}

fn main() -> anyhow::Result<ExitCode> {
    let args = Args::parse();

    match args {
        Args::Build(args) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .compact()
                        .with_target(false)
                        .without_time(),
                )
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                        tracing_subscriber::EnvFilter::new("brioche=info,warn")
                    }),
                )
                .init();

            // let js_platform = brioche_core::script::initialize_js_platform();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(build::build(args))?;

            Ok(exit_code)
        }
        Args::WeirdUi(args) => {
            cfg_select! {
                feature = "weird-ui" => {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()?;
                    rt.block_on(weird_ui::launch_weird_ui(args))?;
                    Ok(ExitCode::SUCCESS)
                }
                _ => {
                    let _ = args;
                    anyhow::bail!("Brioche weird-ui feature was disabled at compile-time");
                }
            }
        }
        Args::RunSandbox(args) => {
            let exit_code = run_sandbox::run_sandbox(&args);

            Ok(exit_code)
        }
    }
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

#[derive(Debug, Parser)]
pub struct WeirdUiArgs {
    /// Projects to build (e.g., `./pkg`, `curl`, `./pkg^test`, `^test`, `curl^test,default`).
    #[arg(value_parser = ProjectRefsParser, conflicts_with_all = ["project", "registry", "export"])]
    targets: Vec<ProjectRefs>,

    /// Deprecated: use positional arguments instead.
    #[arg(short, long, hide = true, conflicts_with = "registry")]
    project: Option<PathBuf>,

    /// Deprecated: use positional arguments instead.
    #[arg(short, long, hide = true)]
    registry: Option<String>,

    /// Deprecated: use positional arguments instead.
    #[arg(short, long, hide = true)]
    export: Option<String>,

    /// Check the project before building.
    #[arg(long)]
    check: bool,

    /// Validate that the lockfile is up-to-date.
    #[arg(long)]
    locked: bool,
}
