use std::{collections::HashSet, process::ExitCode};

use anyhow::Context as _;
use brioche_core::{project::ProjectLocking, utils::DisplayDuration};
use clap::Parser;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use tracing::Instrument as _;

#[derive(Debug, Parser)]
pub struct RunArgs {
    #[command(flatten)]
    project: super::ProjectArgs,

    /// Which TypeScript export to build
    #[arg(short, long, default_value = "default")]
    export: String,

    /// The path within the build artifact to execute
    #[arg(short, long, default_value = "brioche-run")]
    command: String,

    /// Suppress Brioche's output
    #[arg(short, long)]
    quiet: bool,

    /// Check the project before building
    #[arg(long)]
    check: bool,

    /// Validate that the lockfile is up-to-date
    #[arg(long)]
    locked: bool,

    /// Keep temporary build files. Useful for debugging build failures
    #[arg(long)]
    keep_temps: bool,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,

    /// Arguments to pass to the command
    #[arg(last = true)]
    args: Vec<std::ffi::OsString>,
}

#[expect(clippy::print_stderr)]
pub async fn run(
    js_platform: brioche_core::script::JsPlatform,
    args: RunArgs,
) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = if args.quiet {
        brioche_core::reporter::start_null_reporter()
    } else {
        brioche_core::reporter::console::start_console_reporter(
            args.display.to_console_reporter_kind(),
        )?
    };

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .keep_temps(args.keep_temps)
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

    let projects = brioche_core::project::Projects::default();

    let locking = if args.locked {
        ProjectLocking::Locked
    } else {
        ProjectLocking::Unlocked
    };

    let build_future = async {
        let project_hash = super::load_project(&brioche, &projects, &args.project, locking).await?;

        // If the `--locked` flag is used, validate that all lockfiles are
        // up-to-date. Otherwise, write any out-of-date lockfiles
        if args.locked {
            projects.validate_no_dirty_lockfiles()?;
        } else {
            let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
            if num_lockfiles_updated > 0 {
                tracing::info!(num_lockfiles_updated, "updated lockfiles");
            }
        }

        if args.check {
            let project_hashes = HashSet::from_iter([project_hash]);

            let checked = brioche_core::script::check::check(
                &brioche,
                js_platform,
                &projects,
                &project_hashes,
            )
            .await?;

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
            js_platform,
            &projects,
            project_hash,
            &args.export,
        )
        .await?;

        let artifact = brioche_core::bake::bake(
            &brioche,
            recipe,
            &brioche_core::bake::BakeScope::Project {
                project_hash,
                export: args.export.to_string(),
            },
        )
        .instrument(tracing::info_span!("bake"))
        .await?;

        guard.shutdown_console().await;

        let elapsed = DisplayDuration(reporter.elapsed());
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

        brioche.wait_for_tasks().await;

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
