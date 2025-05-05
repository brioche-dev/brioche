use anyhow::Context as _;
use brioche_core::{project::ProjectLocking, utils::DisplayDuration};
use bstr::ByteSlice;
use clap::Parser;
use tokio::io::AsyncWriteExt;
use tracing::Instrument as _;

#[derive(Debug, Parser)]
pub struct LiveUpdateArgs {
    #[command(flatten)]
    project: super::ProjectArgs,

    /// Check the project before building
    #[arg(long)]
    check: bool,

    /// Validate that the lockfile is up-to-date before applying updates
    #[arg(long)]
    locked: bool,

    /// Keep temporary build files. Useful for debugging build failures
    #[arg(long)]
    keep_temps: bool,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

#[expect(clippy::print_stderr)]
pub async fn live_update(
    js_platform: brioche_core::script::JsPlatform,
    args: LiveUpdateArgs,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.project.registry.is_none(),
        "cannot edit a registry project"
    );

    let (reporter, mut guard) = brioche_core::reporter::console::start_console_reporter(
        args.display.to_console_reporter_kind(),
    )?;

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

    let build_future = async {
        if args.check {
            let checked =
                brioche_core::script::check::check(&brioche, js_platform, &projects, project_hash)
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
            "liveUpdate",
        )
        .await?;

        let artifact = brioche_core::bake::bake(
            &brioche,
            recipe,
            &brioche_core::bake::BakeScope::Project {
                project_hash,
                export: "liveUpdate".to_string(),
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
        eprintln!("Build finished, completed {jobs_message} in {elapsed}");

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
                .get(&brioche, b"brioche-run")
                .await
                .with_context(|| "failed to retrieve \"brioche-run\" from returned artifact")?,
        };
        anyhow::ensure!(
            command_artifact.is_some(),
            "\"brioche-run\" not found in returned artifact",
        );

        let output = brioche_core::output::create_local_output(&brioche, &artifact.value).await?;

        brioche.wait_for_tasks().await;

        Ok(output)
    };

    let output = build_future
        .instrument(tracing::info_span!("run_build"))
        .await?;

    let command_path = output.path.join("brioche-run");

    eprintln!("Executing brioche-run");

    let mut command = tokio::process::Command::new(command_path);

    if let Some(resource_dir) = output.resource_dir {
        command.env("BRIOCHE_RESOURCE_DIR", resource_dir);
    }

    let output = command.output().await.context("failed to run process")?;

    let stderr = bstr::BStr::new(&output.stderr).trim_end();
    eprintln!("{}", bstr::BStr::new(stderr));

    if !output.status.success() {
        let stdout = bstr::BStr::new(&output.stdout).trim_end();
        eprintln!("{}", bstr::BStr::new(stdout));

        anyhow::bail!("process failed: {}", output.status);
    }

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "failed to parse JSON response: {:?}",
            bstr::BStr::new(&output.stdout)
        )
    })?;
    let project_paths = projects.local_paths(project_hash)?;

    anyhow::ensure!(project_paths.len() == 1, "could not determine project path");
    let project_path = project_paths.iter().next().unwrap();

    let did_update = brioche_core::project::edit::edit_project(
        &brioche.vfs,
        project_path,
        brioche_core::project::edit::ProjectChanges {
            project_definition: Some(value),
        },
    )
    .await?;

    if did_update {
        // Reload the project from scratch
        let projects = brioche_core::project::Projects::default();
        super::load_project(&brioche, &projects, &args.project, ProjectLocking::Unlocked).await?;

        // Update lockfiles
        projects.commit_dirty_lockfiles().await?;

        eprintln!("Updated project");
    } else {
        eprintln!("Project is already up-to-date");
    }

    Ok(())
}
