use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use brioche_core::Brioche;
use brioche_core::project::ProjectHash;
use brioche_core::project::ProjectLocking;
use brioche_core::project::ProjectValidation;
use brioche_core::project::Projects;
use brioche_core::reporter::Reporter;
use brioche_core::utils::DisplayDuration;
use clap::Parser;
use tracing::Instrument as _;

use crate::consolidate_result;

#[derive(Debug, Parser)]
pub struct InstallArgs {
    #[command(flatten)]
    project: super::MultipleProjectArgs,

    /// Which TypeScript export to build
    #[arg(short, long, default_value = "default")]
    export: String,

    /// Check the project before building
    #[arg(long)]
    check: bool,

    /// Validate that the lockfile is up-to-date
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

    let projects = brioche_core::project::Projects::default();

    let install_options = InstallOptions {
        check: args.check,
        locked: args.locked,
    };
    let locking = if args.locked {
        ProjectLocking::Locked
    } else {
        ProjectLocking::Unlocked
    };
    let mut error_result = Option::None;

    // Handle the case where no projects and no registries are specified
    let projects_path =
        if args.project.project.is_empty() && args.project.registry_project.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            args.project.project
        };

    // Loop over the projects
    for project_path in projects_path {
        let project_name = format!("project '{name}'", name = project_path.display());

        match projects
            .load(
                &brioche,
                &project_path,
                ProjectValidation::Standard,
                locking,
            )
            .await
        {
            Ok(project_hash) => {
                let result = run_install(
                    &reporter,
                    &brioche,
                    js_platform,
                    &projects,
                    project_hash,
                    &project_name,
                    &args.export,
                    &install_options,
                )
                .await;

                consolidate_result(&reporter, &project_name, result, &mut error_result);
            }
            Err(e) => {
                consolidate_result(&reporter, &project_name, Err(e), &mut error_result);
            }
        }
    }

    // Loop over the registry projects
    for registry_project in args.project.registry_project {
        let project_name = format!("registry project '{registry_project}'");

        match projects
            .load_from_registry(
                &brioche,
                &registry_project,
                &brioche_core::project::Version::Any,
            )
            .await
        {
            Ok(project_hash) => {
                let result = run_install(
                    &reporter,
                    &brioche,
                    js_platform,
                    &projects,
                    project_hash,
                    &project_name,
                    &args.export,
                    &install_options,
                )
                .await;

                consolidate_result(&reporter, &project_name, result, &mut error_result);
            }
            Err(e) => {
                consolidate_result(&reporter, &project_name, Err(e), &mut error_result);
            }
        }
    }

    guard.shutdown_console().await;
    brioche.wait_for_tasks().await;

    let exit_code = if error_result.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };

    Ok(exit_code)
}

struct InstallOptions {
    check: bool,
    locked: bool,
}

#[expect(clippy::too_many_arguments)]
async fn run_install(
    reporter: &Reporter,
    brioche: &Brioche,
    js_platform: brioche_core::script::JsPlatform,
    projects: &Projects,
    project_hash: ProjectHash,
    project_name: &String,
    export: &String,
    options: &InstallOptions,
) -> Result<bool, anyhow::Error> {
    async {
        // If the `--locked` flag is used, validate that all lockfiles are
        // up-to-date. Otherwise, write any out-of-date lockfiles
        if options.locked {
            projects.validate_no_dirty_lockfiles()?;
        } else {
            let num_lockfiles_updated = projects.commit_dirty_lockfiles().await?;
            if num_lockfiles_updated > 0 {
                tracing::info!(num_lockfiles_updated, "updated lockfiles");
            }
        }

        if options.check {
            let checked = brioche_core::script::check::check(
                brioche,
                js_platform,
                projects,
                &HashSet::from_iter([project_hash]),
            )
            .await?;

            let result = checked.ensure_ok(brioche_core::script::check::DiagnosticLevel::Error);

            match result {
                Ok(()) => reporter.emit(superconsole::Lines::from_multiline_string(
                    &format!("No errors found in {project_name}"),
                    superconsole::style::ContentStyle::default(),
                )),
                Err(diagnostics) => {
                    let mut output = Vec::new();
                    diagnostics.write(&brioche.vfs, &mut output)?;

                    reporter.emit(superconsole::Lines::from_multiline_string(
                        &String::from_utf8(output)?,
                        superconsole::style::ContentStyle::default(),
                    ));

                    return Ok(false);
                }
            }
        }

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
                export: export.to_string(),
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
            &format!("Build finished, completed {jobs_message} in {elapsed}",),
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
