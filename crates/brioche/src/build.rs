use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use brioche_core::project::{ProjectHash, ProjectLocking, ProjectValidation, Projects};
use brioche_core::reporter::Reporter;
use brioche_core::utils::DisplayDuration;
use brioche_core::{Brioche, fs_utils};
use clap::Parser;
use tracing::Instrument as _;

use crate::utils::{
    ProjectRef, ProjectRefs, ProjectRefsParser, ProjectSource, consolidate_result,
    numbered_output_paths, resolve_project_refs,
};

#[derive(Debug, Parser)]
pub struct BuildArgs {
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

    /// The path to write the output to. The build result will not be
    /// saved if not specified.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Check the project before building.
    #[arg(long)]
    check: bool,

    /// Validate that the lockfile is up-to-date.
    #[arg(long)]
    locked: bool,

    /// Replace the output path if it already exists.
    #[arg(long)]
    replace: bool,

    /// Merge the output path if it already exists.
    #[arg(long)]
    merge: bool,

    /// Keep temporary build files. Useful for debugging build failures.
    #[arg(long)]
    keep_temps: bool,

    /// Sync / cache baked recipes to the registry during the build.
    #[arg(long)]
    sync: bool,

    /// (Experimental!) If the build result is found in the remote cache, exit
    /// early without fetching the build result. Conflicts with `--output`.
    #[arg(long, conflicts_with = "output")]
    experimental_lazy: bool,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

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

    let project_refs = resolve_project_refs(args.targets, args.project, args.registry, args.export);

    // Compute numbered output paths if --output is set
    let output_paths = args
        .output
        .as_ref()
        .map(|base| numbered_output_paths(base, project_refs.len()));

    let projects = brioche_core::project::Projects::default();

    let locking = if args.locked {
        ProjectLocking::Locked
    } else {
        ProjectLocking::Unlocked
    };

    let build_result = async {
        let mut error_result = None;

        // Load projects and pair each ref with its resolved hash
        let mut load_cache: HashMap<_, _> = HashMap::new();
        let mut projects_resolved = Vec::with_capacity(project_refs.len());

        for project_ref in &project_refs {
            if let Some(&hash) = load_cache.get(&project_ref.source) {
                projects_resolved.push((hash, project_ref));
                continue;
            }
            let result =
                load_project_source(&brioche, &projects, &project_ref.source, locking).await;
            match result {
                Ok(hash) => {
                    load_cache.insert(&project_ref.source, hash);
                    projects_resolved.push((hash, project_ref));
                }
                Err(err) => {
                    let name = project_ref.source.to_string();
                    consolidate_result(&reporter, Some(&name), Err(err), &mut error_result);
                }
            }
        }
        drop(load_cache);

        // If any project failed to load, skip remaining phases
        if error_result.is_some() {
            return anyhow::Ok(None);
        }

        // Lockfile handling
        if args.locked {
            projects.validate_no_dirty_lockfiles()?;
        } else {
            let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
            if num_lockfiles_updated > 0 {
                tracing::info!(num_lockfiles_updated, "updated lockfiles");
            }
        }

        // Check (if --check): batch all loaded project hashes
        if args.check {
            let project_hashes = projects_resolved
                .iter()
                .map(|(hash, _)| *hash)
                .collect::<HashSet<_>>();

            if !project_hashes.is_empty() {
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
                        return anyhow::Ok(None);
                    }
                }
            }
        }

        // Experimental lazy path: check if all targets are cached
        if args.experimental_lazy {
            let mut all_cached = true;

            for &(project_hash, ProjectRef { source: _, export }) in &projects_resolved {
                let Ok(recipe) = brioche_core::script::evaluate::evaluate(
                    &brioche,
                    js_platform,
                    &projects,
                    project_hash,
                    export,
                )
                .await
                else {
                    all_cached = false;
                    break;
                };

                let lazy_bake_start = std::time::Instant::now();

                let is_cheap_or_cached =
                    brioche_core::lazy_bake::is_cheap_to_bake_or_in_remote_cache(
                        &brioche,
                        recipe.value.clone(),
                    )
                    .await
                    .inspect_err(|error| {
                        tracing::warn!(
                            "encountered error while checking remote cache for bake: {error:#}"
                        );
                    })
                    .unwrap_or(false);

                let lazy_bake_elapsed = DisplayDuration(lazy_bake_start.elapsed());
                tracing::debug!("checked cache for lazy bake result in {lazy_bake_elapsed}");

                if !is_cheap_or_cached {
                    all_cached = false;
                    break;
                }
            }

            if all_cached {
                let elapsed = DisplayDuration(reporter.elapsed());
                reporter.emit(superconsole::Lines::from_multiline_string(
                    &format!("Lazy: found all recipe inputs in cache in {elapsed}"),
                    superconsole::style::ContentStyle::default(),
                ));
                return Ok(Some(()));
            }
        }

        // Build loop
        for (i, &(project_hash, ProjectRef { source, export })) in
            projects_resolved.iter().enumerate()
        {
            let output_path = output_paths.as_ref().map(|paths| &paths[i]);
            let project_name = source.to_string();

            let build_opts = BuildTargetOptions {
                project_hash,
                export,
                output: output_path,
                replace: args.replace,
                merge: args.merge,
                sync: args.sync,
            };

            let result =
                run_build_target(&brioche, js_platform, &projects, &reporter, &build_opts).await;

            consolidate_result(&reporter, Some(&project_name), result, &mut error_result);
        }

        brioche.wait_for_tasks().await;
        anyhow::Ok(error_result)
    }
    .instrument(tracing::info_span!("build"))
    .await;

    guard.shutdown_console().await;

    let error_result = build_result?;
    let exit_code = error_result.map_or(ExitCode::SUCCESS, |()| ExitCode::FAILURE);

    Ok(exit_code)
}

async fn load_project_source(
    brioche: &Brioche,
    projects: &Projects,
    source: &ProjectSource,
    locking: ProjectLocking,
) -> anyhow::Result<ProjectHash> {
    match source {
        ProjectSource::Local(path) => {
            projects
                .load(brioche, path, ProjectValidation::Standard, locking)
                .await
        }
        ProjectSource::Registry(name) => {
            projects
                .load_from_registry(brioche, name, &brioche_core::project::Version::Any)
                .await
        }
    }
}

struct BuildTargetOptions<'a> {
    project_hash: ProjectHash,
    export: &'a str,
    output: Option<&'a PathBuf>,
    replace: bool,
    merge: bool,
    sync: bool,
}

async fn run_build_target(
    brioche: &Brioche,
    js_platform: brioche_core::script::JsPlatform,
    projects: &Projects,
    reporter: &Reporter,
    options: &BuildTargetOptions<'_>,
) -> Result<bool, anyhow::Error> {
    let recipe = brioche_core::script::evaluate::evaluate(
        brioche,
        js_platform,
        projects,
        options.project_hash,
        options.export,
    )
    .await?;

    let artifact = brioche_core::bake::bake(
        brioche,
        recipe,
        &brioche_core::bake::BakeScope::Project {
            project_hash: options.project_hash,
            export: options.export.to_owned(),
        },
    )
    .instrument(tracing::info_span!("bake"))
    .await?;

    let artifact_hash = artifact.value.hash();
    let default_style = superconsole::style::ContentStyle::default();

    let elapsed = DisplayDuration(reporter.elapsed());
    let num_jobs = reporter.num_jobs();
    let jobs_message = match num_jobs {
        0 => "(no new jobs)".to_string(),
        1 => "1 job".to_string(),
        n => format!("{n} jobs"),
    };
    reporter.emit(superconsole::Lines::from_multiline_string(
        &format!("Build finished, completed {jobs_message} in {elapsed}"),
        default_style,
    ));

    reporter.emit(superconsole::Lines::from_multiline_string(
        &format!("Result: {artifact_hash}"),
        default_style,
    ));

    if let Some(output) = options.output {
        if options.replace {
            fs_utils::try_remove(output)
                .await
                .with_context(|| format!("Failed to remove path {}", output.display()))?;
        }

        reporter.emit(superconsole::Lines::from_multiline_string(
            &format!("Writing output to {}", output.display()),
            default_style,
        ));

        brioche_core::output::create_output(
            brioche,
            &artifact.value,
            brioche_core::output::OutputOptions {
                output_path: output,
                merge: options.merge,
                resource_dir: None,
                mtime: Some(std::time::SystemTime::now()),
                link_locals: false,
            },
        )
        .await?;

        reporter.emit(superconsole::Lines::from_multiline_string(
            &format!("Wrote output to {}", output.display()),
            default_style,
        ));
    }

    if options.sync {
        reporter.emit(superconsole::Lines::from_multiline_string(
            "Waiting for in-progress syncs to finish...",
            default_style,
        ));

        let wait_start = std::time::Instant::now();

        let brioche_core::sync::SyncBakesResults {
            num_new_recipes,
            num_new_bakes,
        } = brioche_core::sync::wait_for_in_progress_syncs(brioche).await?;

        let wait_duration = DisplayDuration(wait_start.elapsed());
        reporter.emit(superconsole::Lines::from_multiline_string(
            &format!(
                "In-progress sync waited for {wait_duration} and synced:\n  {num_new_recipes} recipes\n  {num_new_bakes} bakes"
            ),
            default_style,
        ));

        reporter.emit(superconsole::Lines::from_multiline_string(
            "Syncing project...",
            default_style,
        ));

        let sync_start = std::time::Instant::now();
        brioche_core::sync::sync_project(brioche, options.project_hash, options.export).await?;
        let sync_duration = DisplayDuration(sync_start.elapsed());
        reporter.emit(superconsole::Lines::from_multiline_string(
            &format!("Finished sync in {sync_duration}"),
            default_style,
        ));
    }

    Ok(true)
}
