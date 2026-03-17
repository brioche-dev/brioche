use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use brioche_core::Brioche;
use brioche_core::project::{ProjectHash, ProjectLocking, Projects};
use brioche_core::reporter::Reporter;
use brioche_core::utils::DisplayDuration;
use clap::Parser;
use tracing::Instrument as _;

use crate::utils::{
    ProjectRef, ProjectRefs, ProjectRefsParser, consolidate_result, load_project_source,
    resolve_project_refs,
};

#[derive(Debug, Parser)]
pub struct InstallArgs {
    /// Projects to install (e.g., `./pkg`, `curl`, `./pkg^test`, `^test`, `curl^test,default`).
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

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

pub async fn install(
    js_platform: brioche_core::script::JsPlatform,
    args: InstallArgs,
) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = brioche_core::reporter::console::start_console_reporter(
        args.display.to_console_reporter_kind(),
    )?;

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

    let project_refs = resolve_project_refs(args.targets, args.project, args.registry, args.export);

    let projects = brioche_core::project::Projects::default();

    let locking = if args.locked {
        ProjectLocking::Locked
    } else {
        ProjectLocking::Unlocked
    };

    let mut error_result = None;

    // Load projects and pair each ref with its resolved hash
    let mut load_cache: HashMap<_, _> = HashMap::new();
    let mut projects_resolved = Vec::with_capacity(project_refs.len());

    for project_ref in &project_refs {
        if let Some(&hash) = load_cache.get(&project_ref.source) {
            projects_resolved.push((hash, project_ref));
            continue;
        }
        let result = load_project_source(&brioche, &projects, &project_ref.source, locking).await;
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

    if error_result.is_some() {
        guard.shutdown_console().await;
        brioche.wait_for_tasks().await;
        return Ok(ExitCode::FAILURE);
    }

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

                    let mut output = Vec::new();
                    diagnostics.write(&brioche.vfs, &mut output)?;

                    reporter.emit(superconsole::Lines::from_multiline_string(
                        &String::from_utf8(output)?,
                        superconsole::style::ContentStyle::default(),
                    ));

                    brioche.wait_for_tasks().await;
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
    }

    // Install loop
    for &(project_hash, ProjectRef { source, export }) in &projects_resolved {
        let project_name = source.to_string();

        let result = run_install(
            &reporter,
            &brioche,
            js_platform,
            &projects,
            project_hash,
            &project_name,
            export,
        )
        .await;

        consolidate_result(&reporter, Some(&project_name), result, &mut error_result);
    }

    guard.shutdown_console().await;
    brioche.wait_for_tasks().await;

    let exit_code = error_result.map_or(ExitCode::SUCCESS, |()| ExitCode::FAILURE);

    Ok(exit_code)
}

async fn run_install(
    reporter: &Reporter,
    brioche: &Brioche,
    js_platform: brioche_core::script::JsPlatform,
    projects: &Projects,
    project_hash: ProjectHash,
    project_name: &str,
    export: &str,
) -> Result<bool, anyhow::Error> {
    async {
        let recipe = brioche_core::script::evaluate::evaluate(
            brioche,
            js_platform,
            projects,
            project_hash,
            export,
        )
        .await?;

        let artifact = brioche_core::bake::bake(
            brioche,
            recipe,
            &brioche_core::bake::BakeScope::Project {
                project_hash,
                export: export.to_owned(),
            },
        )
        .instrument(tracing::info_span!("bake"))
        .await?;

        let elapsed = DisplayDuration(reporter.elapsed());
        let num_jobs = reporter.num_jobs();
        let jobs_message = match num_jobs {
            0 => "(no new jobs)".to_string(),
            1 => "1 job".to_string(),
            n => format!("{n} jobs"),
        };

        reporter.emit(superconsole::Lines::from_multiline_string(
            &format!("Build finished, completed {jobs_message} in {elapsed}"),
            superconsole::style::ContentStyle::default(),
        ));

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
        directory.insert(brioche, b"brioche-run", None).await?;

        // Create the installation directory if it doesn't exist
        let install_dir = brioche.data_dir.join("installed");
        tokio::fs::create_dir_all(&install_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to create installation directory {}",
                    install_dir.display()
                )
            })?;

        reporter.emit(superconsole::Lines::from_multiline_string(
            &format!("Writing output for {project_name}"),
            superconsole::style::ContentStyle::default(),
        ));
        brioche_core::output::create_output(
            brioche,
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
        reporter.emit(superconsole::Lines::from_multiline_string(
            &format!("Wrote output to {}", install_dir.display()),
            superconsole::style::ContentStyle::default(),
        ));

        let install_bin_dir = install_dir.join("bin");
        let install_bin_dir_exists = tokio::fs::try_exists(&install_bin_dir).await?;
        let is_on_path = std::env::var_os("PATH").is_some_and(|env_path| {
            let mut paths = std::env::split_paths(&env_path);
            paths.any(|path| path == install_bin_dir)
        });

        if install_bin_dir_exists && !is_on_path {
            reporter.emit(superconsole::Lines::from_multiline_string(
                &format!(
                    "Note: installation directory not detected in $PATH! Consider adding it:\n  {}",
                    install_bin_dir.display()
                ),
                superconsole::style::ContentStyle::default(),
            ));
        }

        Ok(true)
    }
    .instrument(tracing::info_span!("run_install"))
    .await
}
