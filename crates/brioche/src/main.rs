use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

mod build;
mod run_sandbox;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(version)]
enum Args {
    /// Build a project
    Build(build::BuildArgs),

    /// Used by Brioche itself to run a sandboxed process
    #[command(hide = true)]
    RunSandbox(run_sandbox::RunSandboxArgs),
}

#[expect(clippy::print_stdout)]
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
