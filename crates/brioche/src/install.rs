use std::process::ExitCode;

use anyhow::Context as _;
use brioche_core::reporter::ConsoleReporterKind;
use clap::Parser;
use human_repr::HumanDuration;
use tracing::Instrument;

#[derive(Debug, Parser)]
pub struct InstallArgs {
    #[command(flatten)]
    project: super::ProjectArgs,

    /// Which TypeScript export to build
    #[arg(short, long, default_value = "default")]
    export: String,

    /// Check the project before buiilding
    #[arg(long)]
    check: bool,
}

pub async fn install(args: InstallArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) =
        brioche_core::reporter::start_console_reporter(ConsoleReporterKind::Auto)?;
    reporter.set_is_evaluating(true);

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    let projects = brioche_core::project::Projects::default();

    let install_future = async {
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
