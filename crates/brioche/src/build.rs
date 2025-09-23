use std::{collections::HashSet, hash::RandomState, path::PathBuf, process::ExitCode};

use anyhow::Context as _;
use brioche_core::{fs_utils, project::ProjectLocking, utils::DisplayDuration};
use clap::Parser;
use tracing::Instrument as _;

#[derive(Debug, Parser)]
pub struct BuildArgs {
    #[command(flatten)]
    project: super::ProjectArgs,

    /// Which TypeScript export to build
    #[arg(short, long, default_value = "default")]
    export: String,

    /// The path to write the output to. The build result will not be
    /// saved if not specified
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Check the project before building
    #[arg(long)]
    check: bool,

    /// Validate that the lockfile is up-to-date
    #[arg(long)]
    locked: bool,

    /// Replace the output path if it already exists
    #[arg(long)]
    replace: bool,

    /// Merge the output path if it already exists
    #[arg(long)]
    merge: bool,

    /// Keep temporary build files. Useful for debugging build failures
    #[arg(long)]
    keep_temps: bool,

    /// Sync / cache baked recipes to the registry during the build
    #[arg(long)]
    sync: bool,

    /// (Experimental!) If the build result is found in the remote cache, exit
    /// early without fetching the build result. Conflicts with `--output`
    #[arg(long)]
    experimental_lazy: bool,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

#[expect(clippy::print_stdout)]
pub async fn build(
    js_platform: brioche_core::script::JsPlatform,
    args: BuildArgs,
) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = brioche_core::reporter::console::start_console_reporter(
        args.display.to_console_reporter_kind(),
    )?;

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .keep_temps(args.keep_temps)
        .sync(args.sync)
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

    anyhow::ensure!(
        !args.experimental_lazy || args.output.is_none(),
        "cannot use both `--experimental-lazy` and `--output` options at the same time"
    );

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
            let project_hashes: HashSet<_, RandomState> = HashSet::from_iter([project_hash]);

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
                    return anyhow::Ok(ExitCode::FAILURE);
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

        if args.experimental_lazy {
            let lazy_bake_start = std::time::Instant::now();

            let is_cheap_or_cached = brioche_core::lazy_bake::is_cheap_to_bake_or_in_remote_cache(
                &brioche,
                recipe.value.clone(),
            )
            .await
            .inspect_err(|error| {
                tracing::warn!("encountered error while checking remote cache for bake: {error:#}");
            })
            .unwrap_or(false);

            let lazy_bake_elapsed = DisplayDuration(lazy_bake_start.elapsed());
            tracing::debug!("checked cache for lazy bake result in {lazy_bake_elapsed}");

            if is_cheap_or_cached {
                guard.shutdown_console().await;

                let elapsed = DisplayDuration(reporter.elapsed());

                println!("Lazy: found all recipe inputs in cache in {elapsed}");
                return Ok(ExitCode::SUCCESS);
            }
        }

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

            let brioche_core::sync::SyncBakesResults {
                num_new_recipes,
                num_new_bakes,
            } = brioche_core::sync::wait_for_in_progress_syncs(&brioche).await?;

            let wait_duration = DisplayDuration(wait_start.elapsed());
            println!("In-progress sync waited for {wait_duration} and synced:");
            println!("  {num_new_recipes} recipes");
            println!("  {num_new_bakes} bakes");

            println!("Syncing project...");

            let sync_start = std::time::Instant::now();
            brioche_core::sync::sync_project(&brioche, project_hash, &args.export).await?;
            let sync_duration = DisplayDuration(sync_start.elapsed());
            println!("Finished sync in {sync_duration}");
        }

        brioche.wait_for_tasks().await;

        anyhow::Ok(ExitCode::SUCCESS)
    };

    let exit_code = build_future
        .instrument(tracing::info_span!("build"))
        .await?;

    Ok(exit_code)
}
