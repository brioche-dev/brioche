use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use anyhow::Context as _;
use brioche_core::{fs_utils, reporter::ConsoleReporterKind, sandbox::SandboxExecutionConfig};
use clap::Parser;
use human_repr::HumanDuration;
use tracing::Instrument;

#[derive(Debug, Parser)]
#[command(version)]
enum Args {
    Build(BuildArgs),

    Run(RunArgs),

    Install(InstallArgs),

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
        Args::Install(args) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;

            let exit_code = rt.block_on(install(args))?;

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
    project: Option<PathBuf>,
    #[arg(short, long)]
    registry: Option<String>,
    #[arg(short, long, default_value = "default")]
    export: String,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long)]
    check: bool,
    #[arg(long)]
    replace: bool,
    #[arg(long)]
    merge: bool,
    #[arg(long)]
    keep_temps: bool,
    #[arg(long)]
    sync: bool,
}

async fn build(args: BuildArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;
    reporter.set_is_evaluating(true);

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .keep_temps(args.keep_temps)
        .sync(args.sync)
        .build()
        .await?;
    let projects = brioche_core::project::Projects::default();

    let build_future = async {
        let project_hash = load_project(
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

        if args.check {
            let checked =
                brioche_core::script::check::check(&brioche, &projects, project_hash).await?;

            let result = checked.ensure_ok(brioche_core::script::check::DiagnosticLevel::Error);

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

        let recipe = brioche_core::script::evaluate::evaluate(
            &brioche,
            &projects,
            project_hash,
            &args.export,
        )
        .await?;

        reporter.set_is_evaluating(false);
        let artifact = brioche_core::bake::bake(
            &brioche,
            recipe,
            &brioche_core::bake::BakeScope::Project {
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
            brioche_core::output::create_output(
                &brioche,
                &artifact.value,
                brioche_core::output::OutputOptions {
                    output_path: output,
                    merge: args.merge,
                    resource_dir: None,
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
                .send(brioche_core::SyncMessage::Flush {
                    completed: sync_complete_tx,
                })
                .await?;
            let brioche_core::sync::SyncBakesResults {
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
            brioche_core::sync::sync_project(&brioche, project_hash, &args.export).await?;
            let sync_duration = sync_start.elapsed().human_duration();
            println!("Finished sync in {sync_duration}");
        }

        anyhow::Ok(ExitCode::SUCCESS)
    };

    let exit_code = build_future
        .instrument(tracing::info_span!("build"))
        .await?;

    Ok(exit_code)
}

#[derive(Debug, Parser)]
struct RunArgs {
    #[arg(short, long)]
    project: Option<PathBuf>,
    #[arg(short, long)]
    registry: Option<String>,
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
    #[arg(last = true)]
    args: Vec<std::ffi::OsString>,
}

async fn run(args: RunArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = if args.quiet {
        brioche_core::reporter::start_null_reporter()
    } else {
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?
    };
    reporter.set_is_evaluating(true);

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .keep_temps(args.keep)
        .build()
        .await?;
    let projects = brioche_core::project::Projects::default();

    let build_future = async {
        let project_hash = load_project(
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

        if args.check {
            let checked =
                brioche_core::script::check::check(&brioche, &projects, project_hash).await?;

            let result = checked.ensure_ok(brioche_core::script::check::DiagnosticLevel::Error);

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

        let recipe = brioche_core::script::evaluate::evaluate(
            &brioche,
            &projects,
            project_hash,
            &args.export,
        )
        .await?;

        reporter.set_is_evaluating(false);
        let artifact = brioche_core::bake::bake(
            &brioche,
            recipe,
            &brioche_core::bake::BakeScope::Project {
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
            brioche_core::recipe::Artifact::File(_) => {
                anyhow::bail!("artifact returned a file, expected a directory");
            }
            brioche_core::recipe::Artifact::Symlink { .. } => {
                anyhow::bail!("artifact returned a symlink, expected a directory");
            }
            brioche_core::recipe::Artifact::Directory(dir) => dir
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

        let output = brioche_core::output::create_local_output(&brioche, &artifact.value).await?;

        Ok(output)
    };

    let output = build_future
        .instrument(tracing::info_span!("run_build"))
        .await?;

    let command_path = output.path.join(&args.command);

    if !args.quiet {
        eprintln!("Running {}", args.command);
    }

    let mut command = std::process::Command::new(command_path);
    command.args(&args.args);

    if let Some(resource_dir) = output.resource_dir {
        command.env("BRIOCHE_RESOURCE_DIR", resource_dir);
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
struct InstallArgs {
    #[arg(short, long)]
    project: Option<PathBuf>,
    #[arg(short, long)]
    registry: Option<String>,
    #[arg(short, long, default_value = "default")]
    export: String,
    #[arg(long)]
    check: bool,
}

async fn install(args: InstallArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;
    reporter.set_is_evaluating(true);

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    let projects = brioche_core::project::Projects::default();

    let install_future = async {
        let project_hash = load_project(
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

        if args.check {
            let checked =
                brioche_core::script::check::check(&brioche, &projects, project_hash).await?;

            let result = checked.ensure_ok(brioche_core::script::check::DiagnosticLevel::Error);

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

        let recipe = brioche_core::script::evaluate::evaluate(
            &brioche,
            &projects,
            project_hash,
            &args.export,
        )
        .await?;

        reporter.set_is_evaluating(false);
        let artifact = brioche_core::bake::bake(
            &brioche,
            recipe,
            &brioche_core::bake::BakeScope::Project {
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
        eprintln!("Build finished, completed {jobs_message} in {elapsed}");

        // Ensure the artifact is a directory
        let mut directory = match artifact.value {
            brioche_core::recipe::Artifact::File(_) => {
                anyhow::bail!("artifact returned a file, expected a directory");
            }
            brioche_core::recipe::Artifact::Symlink { .. } => {
                anyhow::bail!("artifact returned a symlink, expected a directory");
            }
            brioche_core::recipe::Artifact::Directory(dir) => dir,
        };

        // Remove the top-level `brioche-run` file if it exists
        directory.insert(&brioche, b"brioche-run", None).await?;

        // Create the installation directory if it doesn't exist
        let install_dir = brioche.home.join("installed");
        tokio::fs::create_dir_all(&install_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to create installation directory {}",
                    install_dir.display()
                )
            })?;

        println!("Writing output");
        brioche_core::output::create_output(
            &brioche,
            &brioche_core::recipe::Artifact::Directory(directory),
            brioche_core::output::OutputOptions {
                output_path: &install_dir,
                merge: true,
                resource_dir: None,
                mtime: Some(std::time::SystemTime::now()),
                link_locals: false,
            },
        )
        .await?;
        println!("Wrote output to {}", install_dir.display());

        let install_bin_dir = install_dir.join("bin");
        let install_bin_dir_exists = tokio::fs::try_exists(&install_bin_dir).await?;
        let is_on_path = match std::env::var_os("PATH") {
            Some(env_path) => {
                let mut paths = std::env::split_paths(&env_path);
                paths.any(|path| path == install_bin_dir)
            }
            None => false,
        };

        if install_bin_dir_exists && !is_on_path {
            println!("Note: installation directory not detected in $PATH! Consider adding it:");
            println!("  {}", install_bin_dir.display());
        }

        Ok(ExitCode::SUCCESS)
    };

    let exit_code = install_future
        .instrument(tracing::info_span!("run_install"))
        .await?;

    Ok(exit_code)
}

#[derive(Debug, Parser)]
struct CheckArgs {
    #[arg(short, long)]
    project: Option<PathBuf>,
    #[arg(short, long)]
    registry: Option<String>,
}

async fn check(args: CheckArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche_core::project::Projects::default();

    let check_future = async {
        let project_hash = load_project(
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
#[derive(Debug, Parser)]
struct FormatArgs {
    #[arg(long)]
    check: bool,
    #[arg(short, long)]
    project: PathBuf,
}

async fn format(args: FormatArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;

    let brioche = brioche_core::BriocheBuilder::new(reporter).build().await?;
    let projects = brioche_core::project::Projects::default();

    let format_future = async {
        let project_hash = projects.load(&brioche, &args.project, true).await?;

        if args.check {
            let mut unformatted_files =
                brioche_core::script::format::check_format(&projects, project_hash).await?;
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
            brioche_core::script::format::format(&projects, project_hash).await?;

            guard.shutdown_console().await;

            anyhow::Ok(ExitCode::SUCCESS)
        }
    };

    let exit_code = format_future
        .instrument(tracing::info_span!("format"))
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
            let (reporter, _guard) = brioche_core::reporter::start_lsp_reporter(client.clone());
            let brioche = brioche_core::BriocheBuilder::new(reporter)
                .registry_client(brioche_core::registry::RegistryClient::disabled())
                .vfs(brioche_core::vfs::Vfs::mutable())
                .build()
                .await?;
            let projects = brioche_core::project::Projects::default();
            let lsp_server = brioche_core::script::lsp::BriocheLspServer::new(
                local_pool, brioche, projects, client,
            )
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

    let status = match brioche_core::sandbox::run_sandbox(config) {
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
