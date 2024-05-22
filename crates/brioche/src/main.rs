use std::{path::PathBuf, process::ExitCode};

use anyhow::Context as _;
use brioche::{fs_utils, reporter::ConsoleReporterKind, sandbox::SandboxExecutionConfig};
use clap::Parser;
use human_repr::HumanDuration;
use joinery::JoinableIterator as _;
use tracing::Instrument;

#[derive(Debug, Parser)]
#[command(version)]
enum Args {
    Build(BuildArgs),

    Run(RunArgs),

    Check(CheckArgs),

    #[command(name = "fmt")]
    Format(FormatArgs),

    Publish(PublishArgs),

    Lsp(LspArgs),

    Analyze(AnalyzeArgs),

    ExportProject(ExportProjectArgs),

    RunSandbox(RunSandboxArgs),
}

const BRIOCHE_SANDBOX_ERROR_CODE: u8 = 122;

fn main() -> anyhow::Result<ExitCode> {
    let args = Args::parse();

    match args {
        Args::Build(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(build(args))?;

            Ok(exit_code)
        }
        Args::Run(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(run(args))?;

            Ok(exit_code)
        }
        Args::Check(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(check(args))?;

            Ok(exit_code)
        }
        Args::Format(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(format(args))?;

            Ok(exit_code)
        }
        Args::Publish(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(publish(args))?;

            Ok(exit_code)
        }
        Args::Lsp(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            rt.block_on(lsp(args))?;

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
            let exit_code = run_sandbox(args);

            Ok(exit_code)
        }
    }
}

#[derive(Debug, Parser)]
struct BuildArgs {
    #[arg(short, long)]
    project: PathBuf,
    #[arg(short, long, default_value = "default")]
    export: String,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long)]
    check: bool,
    #[arg(long)]
    replace: bool,
    #[arg(long)]
    keep: bool,
    #[arg(long)]
    sync: bool,
}

async fn build(args: BuildArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;
    reporter.set_is_evaluating(true);

    let brioche = brioche::BriocheBuilder::new(reporter.clone())
        .keep_temps(args.keep)
        .sync(args.sync)
        .build()
        .await?;
    let projects = brioche::project::Projects::default();

    let build_future = async {
        let project_hash = projects.load(&brioche, &args.project, true).await?;

        let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
        if num_lockfiles_updated > 0 {
            tracing::info!(num_lockfiles_updated, "updated lockfiles");
        }

        if args.check {
            let checked = brioche::script::check::check(&brioche, &projects, project_hash).await?;

            let result = checked.ensure_ok(brioche::script::check::DiagnosticLevel::Error);

            match result {
                Ok(()) => reporter.emit(superconsole::Lines::from_multiline_string(
                    "No errors found",
                    superconsole::style::ContentStyle {
                        foreground_color: Some(superconsole::style::Color::Green),
                        ..superconsole::style::ContentStyle::default()
                    },
                )),
                Err(diagnostics) => {
                    guard.shutdown_console().await;

                    diagnostics.write(&brioche.vfs, &mut std::io::stdout())?;
                    return anyhow::Ok(ExitCode::FAILURE);
                }
            }
        }

        let recipe =
            brioche::script::evaluate::evaluate(&brioche, &projects, project_hash, &args.export)
                .await?;

        reporter.set_is_evaluating(false);
        let artifact = brioche::bake::bake(
            &brioche,
            recipe,
            &brioche::bake::BakeScope::Project {
                project_hash,
                export: args.export.to_string(),
            },
        )
        .await?;

        guard.shutdown_console().await;

        let elapsed = reporter.elapsed().human_duration();
        let num_jobs = reporter.num_jobs();
        let jobs_message = match num_jobs {
            0 => "(no new jobs)".to_string(),
            1 => "1 job".to_string(),
            n => format!("{n} jobs"),
        };
        println!("Build finished, completed {jobs_message} in {elapsed}");

        let artifact_hash = artifact.value.hash();
        println!("Result: {artifact_hash}");

        if let Some(output) = &args.output {
            if args.replace {
                fs_utils::try_remove(output)
                    .await
                    .with_context(|| format!("Failed to remove path {}", output.display()))?;
            }

            println!("Writing output");
            brioche::output::create_output(
                &brioche,
                &artifact.value,
                brioche::output::OutputOptions {
                    output_path: output,
                    merge: false,
                    resources_dir: None,
                    mtime: Some(std::time::SystemTime::now()),
                    link_locals: false,
                },
            )
            .await?;
            println!("Wrote output to {}", output.display());
        }

        if args.sync {
            println!("Waiting for in-progress syncs to finish...");
            let wait_start = std::time::Instant::now();

            let (sync_complete_tx, sync_complete_rx) = tokio::sync::oneshot::channel();

            brioche
                .sync_tx
                .send(brioche::SyncMessage::Flush {
                    completed: sync_complete_tx,
                })
                .await?;
            let brioche::sync::SyncResults {
                num_new_blobs,
                num_new_recipes,
                num_new_bakes,
            } = sync_complete_rx.await?;

            let wait_duration = wait_start.elapsed().human_duration();
            println!("In-progress sync waited for {wait_duration} and synced:");
            println!("  {num_new_blobs} blobs");
            println!("  {num_new_recipes} recipes");
            println!("  {num_new_bakes} bakes");

            println!("Syncing project...");

            let sync_start = std::time::Instant::now();
            brioche::sync::sync_project(&brioche, project_hash, &args.export).await?;
            let sync_duration = sync_start.elapsed().human_duration();
            println!("Finished sync in {sync_duration}");
        }

        anyhow::Ok(ExitCode::SUCCESS)
    };

    let exit_code = build_future
        .instrument(tracing::info_span!("build", args = ?args))
        .await?;

    Ok(exit_code)
}

#[derive(Debug, Parser)]
struct RunArgs {
    #[arg(short, long)]
    project: PathBuf,
    #[arg(short, long, default_value = "default")]
    export: String,
    #[arg(short, long, default_value = "brioche-run")]
    command: String,
    #[arg(short, long)]
    quiet: bool,
    #[arg(long)]
    check: bool,
    #[arg(long)]
    keep: bool,
    args: Vec<std::ffi::OsString>,
}

async fn run(args: RunArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = if args.quiet {
        brioche::reporter::start_null_reporter()
    } else {
        brioche::reporter::start_console_reporter(ConsoleReporterKind::Auto)?
    };
    reporter.set_is_evaluating(true);

    let brioche = brioche::BriocheBuilder::new(reporter.clone())
        .keep_temps(args.keep)
        .build()
        .await?;
    let projects = brioche::project::Projects::default();

    let build_future = async {
        let project_hash = projects.load(&brioche, &args.project, true).await?;

        let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
        if num_lockfiles_updated > 0 {
            tracing::info!(num_lockfiles_updated, "updated lockfiles");
        }

        if args.check {
            let checked = brioche::script::check::check(&brioche, &projects, project_hash).await?;

            let result = checked.ensure_ok(brioche::script::check::DiagnosticLevel::Error);

            match result {
                Ok(()) => reporter.emit(superconsole::Lines::from_multiline_string(
                    "No errors found",
                    superconsole::style::ContentStyle {
                        foreground_color: Some(superconsole::style::Color::Green),
                        ..superconsole::style::ContentStyle::default()
                    },
                )),
                Err(diagnostics) => {
                    guard.shutdown_console().await;

                    diagnostics.write(&brioche.vfs, &mut std::io::stdout())?;
                    anyhow::bail!("checks failed");
                }
            }
        }

        let recipe =
            brioche::script::evaluate::evaluate(&brioche, &projects, project_hash, &args.export)
                .await?;

        reporter.set_is_evaluating(false);
        let artifact = brioche::bake::bake(
            &brioche,
            recipe,
            &brioche::bake::BakeScope::Project {
                project_hash,
                export: args.export.to_string(),
            },
        )
        .await?;

        guard.shutdown_console().await;

        let elapsed = reporter.elapsed().human_duration();
        let num_jobs = reporter.num_jobs();
        let jobs_message = match num_jobs {
            0 => "(no new jobs)".to_string(),
            1 => "1 job".to_string(),
            n => format!("{n} jobs"),
        };
        if !args.quiet {
            eprintln!("Build finished, completed {jobs_message} in {elapsed}");
        }

        // Validate that the artifact is a directory that contains the
        // command to run before returning
        let command_artifact = match &artifact.value {
            brioche::recipe::Artifact::File(_) => {
                anyhow::bail!("artifact returned a file, expected a directory");
            }
            brioche::recipe::Artifact::Symlink { .. } => {
                anyhow::bail!("artifact returned a symlink, expected a directory");
            }
            brioche::recipe::Artifact::Directory(dir) => dir
                .get(&brioche, args.command.as_bytes())
                .await
                .with_context(|| {
                    format!(
                        "failed to retrieve {:?} from returned artifact",
                        args.command
                    )
                })?,
        };
        anyhow::ensure!(
            command_artifact.is_some(),
            "{:?} not found in returned artifact",
            args.command
        );

        let output = brioche::output::create_local_output(&brioche, &artifact.value).await?;

        Ok(output)
    };

    let output = build_future
        .instrument(tracing::info_span!("run_build", args = ?args))
        .await?;

    let command_path = output.path.join(&args.command);

    let mut command = std::process::Command::new(command_path);
    command.args(&args.args);

    if let Some(resources_dir) = output.resources_dir {
        command.env("BRIOCHE_PACK_RESOURCES_DIR", resources_dir);
    }

    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            use std::os::unix::process::CommandExt as _;

            let error = command.exec();
            Err(error.into())
        } else {
            let result = command.status().context("failed to run process")?;
            if result.success() {
                Ok(ExitCode::SUCCESS)
            } else {
                let code = result
                    .code()
                    .and_then(|code| u8::try_from(code).ok())
                    .map(ExitCode::from)
                    .unwrap_or(ExitCode::FAILURE);
                Ok(code)
            }
        }
    }
}

#[derive(Debug, Parser)]
struct CheckArgs {
    #[arg(short, long)]
    project: PathBuf,
}

async fn check(args: CheckArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche::project::Projects::default();

    let check_future = async {
        let project_hash = projects.load(&brioche, &args.project, true).await?;

        let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
        if num_lockfiles_updated > 0 {
            tracing::info!(num_lockfiles_updated, "updated lockfiles");
        }

        let checked = brioche::script::check::check(&brioche, &projects, project_hash).await?;

        guard.shutdown_console().await;

        let result = checked.ensure_ok(brioche::script::check::DiagnosticLevel::Message);

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
        .instrument(tracing::info_span!("check", args = ?args))
        .await?;

    Ok(exit_code)
}
#[derive(Debug, Parser)]
struct FormatArgs {
    #[arg(long)]
    check: bool,
    #[arg(short, long)]
    project: PathBuf,
}

async fn format(args: FormatArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche::project::Projects::default();

    let format_future = async {
        let project_hash = projects.load(&brioche, &args.project, true).await?;

        if args.check {
            let mut unformatted_files =
                brioche::script::format::check_format(&projects, project_hash).await?;
            unformatted_files.sort();

            guard.shutdown_console().await;

            if unformatted_files.is_empty() {
                println!("All files formatted");
                Ok(ExitCode::SUCCESS)
            } else {
                println!("The following files are not formatted:");
                for file in unformatted_files {
                    println!("- {}", file.display());
                }

                Ok(ExitCode::FAILURE)
            }
        } else {
            brioche::script::format::format(&projects, project_hash).await?;

            guard.shutdown_console().await;

            anyhow::Ok(ExitCode::SUCCESS)
        }
    };

    let exit_code = format_future
        .instrument(tracing::info_span!("format", args = ?args))
        .await?;

    Ok(exit_code)
}

#[derive(Debug, Parser)]
struct PublishArgs {
    #[arg(short, long)]
    project: PathBuf,
}

async fn publish(args: PublishArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche::project::Projects::default();
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

    let checked = brioche::script::check::check(&brioche, &projects, project_hash).await?;
    let check_results = checked.ensure_ok(brioche::script::check::DiagnosticLevel::Warning);
    match check_results {
        Ok(()) => {
            println!("No errors found 🎉");
        }
        Err(diagnostics) => {
            diagnostics.write(&brioche.vfs, &mut std::io::stdout())?;
            return Ok(ExitCode::FAILURE);
        }
    }

    let project_listing = projects.export_listing(&brioche, project_hash)?;
    let response = brioche
        .registry_client
        .publish_project(&project_listing)
        .await?;

    guard.shutdown_console().await;

    if response.is_no_op() {
        println!("Project already up to date: {} {}", name, version);
    } else {
        println!("🚀 Published project {} {}", name, version);
        println!("Project hash: {}", project_hash);
        println!("Uploaded files: {}", response.new_files);
        println!("Uploaded projects: {}", response.new_projects);

        if response.tags.is_empty() {
            let tags = response.tags.iter().map(|tag| &tag.name);
            println!("Updated tags: {}", tags.join_with(", "));
        } else {
            println!("No updated tags");
        }
    }

    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Parser)]
struct LspArgs {
    /// Use stdio for LSP transport
    #[arg(long)]
    stdio: bool,
}

async fn lsp(_args: LspArgs) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let local_pool = tokio_util::task::LocalPoolHandle::new(5);

    let (service, socket) = tower_lsp::LspService::new(move |client| {
        let local_pool = &local_pool;
        futures::executor::block_on(async move {
            let (reporter, _guard) = brioche::reporter::start_lsp_reporter(client.clone());
            let brioche = brioche::BriocheBuilder::new(reporter)
                .registry_client(brioche::registry::RegistryClient::disabled())
                .vfs(brioche::vfs::Vfs::mutable())
                .build()
                .await?;
            let projects = brioche::project::Projects::default();
            let lsp_server =
                brioche::script::lsp::BriocheLspServer::new(local_pool, brioche, projects, client)
                    .await?;
            anyhow::Ok(lsp_server)
        })
        .expect("failed to build LSP")
    });

    // Note: For now, we always use stdio for the LSP
    tower_lsp::Server::new(stdin, stdout, socket)
        .serve(service)
        .await;

    Ok(())
}

#[derive(Debug, Parser)]
struct AnalyzeArgs {
    #[arg(short, long)]
    project: PathBuf,
}

async fn analyze(args: AnalyzeArgs) -> anyhow::Result<()> {
    let vfs = brioche::vfs::Vfs::immutable();
    let project = brioche::project::analyze::analyze_project(&vfs, &args.project).await?;
    println!("{project:#?}");
    Ok(())
}

#[derive(Debug, Parser)]
struct ExportProjectArgs {
    #[arg(short, long)]
    project: PathBuf,
}

async fn export_project(args: ExportProjectArgs) -> anyhow::Result<()> {
    let (reporter, mut guard) =
        brioche::reporter::start_console_reporter(ConsoleReporterKind::Plain)?;

    let brioche = brioche::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche::project::Projects::default();
    let project_hash = projects.load(&brioche, &args.project, true).await?;
    let project_listing = projects
        .export_listing(&brioche, project_hash)
        .context("failed to export listing")?;

    guard.shutdown_console().await;

    let serialized = serde_json::to_string_pretty(&project_listing)?;
    println!("{}", serialized);
    Ok(())
}

#[derive(Debug, Parser)]
struct RunSandboxArgs {
    #[arg(long)]
    config: String,
}

fn run_sandbox(args: RunSandboxArgs) -> ExitCode {
    let config = match serde_json::from_str::<SandboxExecutionConfig>(&args.config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("brioche: failed to parse sandbox config: {error:#}");
            return ExitCode::from(BRIOCHE_SANDBOX_ERROR_CODE);
        }
    };

    let status = match brioche::sandbox::run_sandbox(config) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("brioche: failed to run sandbox: {error:#}");
            return ExitCode::from(BRIOCHE_SANDBOX_ERROR_CODE);
        }
    };

    status
        .code()
        .and_then(|code| {
            let code: u8 = code.try_into().ok()?;
            Some(ExitCode::from(code))
        })
        .unwrap_or_else(|| {
            if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        })
}
