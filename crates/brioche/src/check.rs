use std::collections::HashMap;
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
use clap::Parser;
use tracing::Instrument as _;

use crate::consolidate_result;

#[derive(Debug, Parser)]
pub struct CheckArgs {
    /// Project directories to check. Defaults to current directory.
    projects: Vec<PathBuf>,

    /// Deprecated: use positional arguments instead
    #[arg(short, long, hide = true)]
    project: Vec<PathBuf>,

    /// Validate that the lockfile is up-to-date
    #[arg(long)]
    locked: bool,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

pub async fn check(
    js_platform: brioche_core::script::JsPlatform,
    args: CheckArgs,
) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = brioche_core::reporter::console::start_console_reporter(
        args.display.to_console_reporter_kind(),
    )?;

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

    let projects = brioche_core::project::Projects::default();

    let check_options = CheckOptions {
        locked: args.locked,
    };
    let locking = if args.locked {
        ProjectLocking::Locked
    } else {
        ProjectLocking::Unlocked
    };
    let mut error_result = None;

    // Handle the case where no projects and no registries are specified
    let project_paths =
        tokio::task::spawn_blocking(|| resolve_project_paths(args.projects, args.project))
            .await
            .context("failed to resolve project paths")??;

    // Pre-allocate capacity for project names and projects to check
    let mut project_names = HashMap::with_capacity(project_paths.len());
    let mut projects_to_check = HashSet::with_capacity(project_paths.len());

    // Load each path project
    for project_path in project_paths {
        let project_name = format!("project '{name}'", name = project_path.display());
        let project_hash = projects
            .load(
                &brioche,
                &project_path,
                ProjectValidation::Standard,
                locking,
            )
            .await;

        let project_hash = match project_hash {
            Ok(project_hash) => project_hash,
            Err(error) => {
                consolidate_result(
                    &reporter,
                    Some(&project_name),
                    Err(error),
                    &mut error_result,
                );
                continue;
            }
        };

        project_names.entry(project_hash).or_insert(project_name);
        projects_to_check.insert(project_hash);
    }

    // Load each registry project
    for registry_project in args.project.registry_project {
        let project_name = format!("registry project '{registry_project}'");
        let project_hash = projects
            .load_from_registry(
                &brioche,
                &registry_project,
                &brioche_core::project::Version::Any,
            )
            .await;

        let project_hash = match project_hash {
            Ok(project_hash) => project_hash,
            Err(error) => {
                consolidate_result(
                    &reporter,
                    Some(&project_name),
                    Err(error),
                    &mut error_result,
                );
                continue;
            }
        };

        project_names.entry(project_hash).or_insert(project_name);
        projects_to_check.insert(project_hash);
    }

    let project_name = if project_names.len() == 1 {
        Some(project_names.values().next().unwrap())
    } else {
        None
    };
    let project_name = project_name.map(String::as_str);

    let result = run_check(
        &reporter,
        &brioche,
        js_platform,
        &projects,
        &projects_to_check,
        project_name,
        &check_options,
    )
    .await;
    consolidate_result(&reporter, project_name, result, &mut error_result);

    guard.shutdown_console().await;
    brioche.wait_for_tasks().await;

    let exit_code = error_result.map_or(ExitCode::SUCCESS, |()| ExitCode::FAILURE);

    Ok(exit_code)
}

struct CheckOptions {
    locked: bool,
}

/// Resolves input paths by merging positional args with the deprecated `--project` flag,
/// then deduplicates them. Only project directories are accepted — individual files are rejected.
///
/// Returns the current directory as a default when no paths are provided.
fn resolve_project_paths(
    projects: Vec<PathBuf>,
    project: Vec<PathBuf>,
) -> anyhow::Result<HashSet<PathBuf>> {
    if projects.is_empty() && project.is_empty() {
        let path = PathBuf::from(".");
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        return Ok(HashSet::from([path]));
    }

    let all_paths = projects.into_iter().chain(project);
    let mut result = HashSet::with_capacity(all_paths.size_hint().0);

    for path in all_paths {
        if path.is_file() {
            anyhow::bail!(
                "path '{}' is a file, but check only supports project directories",
                path.display()
            );
        }
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        result.insert(path);
    }

    Ok(result)
}

async fn run_check(
    reporter: &Reporter,
    brioche: &Brioche,
    js_platform: brioche_core::script::JsPlatform,
    projects: &Projects,
    project_hashes: &HashSet<ProjectHash>,
    project_name: Option<&str>,
    options: &CheckOptions,
) -> Result<bool, anyhow::Error> {
    let result = async {
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

        brioche_core::script::check::check(brioche, js_platform, projects, project_hashes).await
    }
    .instrument(tracing::info_span!("check"))
    .await?
    .ensure_ok(brioche_core::script::check::DiagnosticLevel::Message);

    match result {
        Ok(()) => {
            let format_string = project_name.map_or_else(
                || "No errors found in projects 🎉".to_string(),
                |project_name| format!("No errors found in {project_name} 🎉"),
            );

            reporter.emit(superconsole::Lines::from_multiline_string(
                &format_string,
                superconsole::style::ContentStyle::default(),
            ));

            Ok(true)
        }
        Err(diagnostics) => {
            let mut output = Vec::new();
            diagnostics.write(&brioche.vfs, &mut output)?;

            reporter.emit(superconsole::Lines::from_multiline_string(
                &String::from_utf8(output)?,
                superconsole::style::ContentStyle::default(),
            ));

            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_inputs_defaults_to_current_dir() {
        let result = resolve_project_paths(vec![], vec![]).unwrap();
        let expected = std::fs::canonicalize(".").unwrap();
        assert_eq!(result, HashSet::from([expected]));
    }

    #[test]
    fn test_deduplicates_paths() {
        let dir = std::env::temp_dir().join("brioche_check_dedup_test");
        std::fs::create_dir_all(&dir).unwrap();

        let path_a = dir.join("./");
        let path_b = dir.clone();
        let result = resolve_project_paths(vec![path_a, path_b], vec![]).unwrap();
        assert_eq!(result.len(), 1);

        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn test_nonexistent_path_accepted() {
        let path = PathBuf::from("this/path/does/not/exist");
        let result = resolve_project_paths(vec![path.clone()], vec![]);
        assert!(result.is_ok());
        assert!(result.unwrap().contains(&path));
    }
}
