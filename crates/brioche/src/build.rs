use std::{path::PathBuf, process::ExitCode};

use anyhow::Context as _;
use brioche_core::{fs_utils, reporter::ConsoleReporterKind};
use clap::Parser;
use human_repr::HumanDuration;
use tracing::Instrument;

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
}

pub async fn build(args: BuildArgs) -> anyhow::Result<ExitCode> {
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
        let project_hash = super::load_project(&brioche, &projects, &args.project).await?;

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

            let brioche_core::sync::SyncBakesResults {
                num_new_blobs,
                num_new_recipes,
                num_new_bakes,
            } = brioche_core::sync::wait_for_in_progress_syncs(&brioche).await?;

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
