use std::collections::HashSet;
use std::{path::PathBuf, process::ExitCode};

use anyhow::Context as _;
use brioche_core::{
    project::{ProjectLocking, ProjectValidation},
    reporter::Reporter,
};
use clap::Parser;
use tracing::Instrument as _;

use crate::consolidate_result;

#[derive(Debug, Parser)]
pub struct FormatArgs {
    /// Paths to format (files or project directories). Defaults to current directory.
    paths: Vec<PathBuf>,

    /// Deprecated: use positional arguments instead
    #[arg(short, long, hide = true)]
    project: Vec<PathBuf>,

    /// Check formatting without writing changes
    #[arg(long)]
    check: bool,

    /// The output display format.
    #[arg(long, value_enum, default_value_t)]
    display: super::DisplayMode,
}

pub async fn format(args: FormatArgs) -> anyhow::Result<ExitCode> {
    let (reporter, mut guard) = brioche_core::reporter::console::start_console_reporter(
        args.display.to_console_reporter_kind(),
    )?;

    let format_result = async {
        let (paths, is_file) =
            tokio::task::spawn_blocking(|| partition_paths(args.paths, args.project))
                .await
                .context("failed to partition paths")??;

        let mut error_result = None;
        let mut all_formatted_files = vec![];

        if is_file {
            format_files(
                &reporter,
                paths,
                args.check,
                &mut error_result,
                &mut all_formatted_files,
            )
            .await;
        } else {
            format_projects(
                &reporter,
                paths,
                args.check,
                &mut error_result,
                &mut all_formatted_files,
            )
            .await?;
        }

        // Report results
        all_formatted_files.sort();
        report_format_results(&reporter, &all_formatted_files, args.check);
        if args.check && !all_formatted_files.is_empty() {
            error_result = Some(());
        }

        anyhow::Ok(error_result)
    }
    .await;

    guard.shutdown_console().await;

    let error_result = format_result?;
    let exit_code = error_result.map_or(ExitCode::SUCCESS, |()| ExitCode::FAILURE);

    Ok(exit_code)
}

/// Resolves input paths by merging positional args with --project flag, then
/// separates them into files and directories. Paths will be deduplicated.
///
/// Returns `(paths, is_file)` where `is_file` indicates if paths are files (`true`) or directories (`false`).
/// Files are validated to have `.bri` extension.
/// Returns an error if both files and directories are specified, or if a file doesn't have `.bri` extension.
fn partition_paths(
    paths: Vec<PathBuf>,
    project: Vec<PathBuf>,
) -> anyhow::Result<(HashSet<PathBuf>, bool)> {
    if paths.is_empty() && project.is_empty() {
        let path = PathBuf::from(".");
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        return Ok((HashSet::from([path]), false));
    }

    // Merge positional args with --project flag
    let all_paths = paths.into_iter().chain(project);

    // Pre-allocate to the final capacity
    let mut result = HashSet::with_capacity(all_paths.size_hint().0);
    let mut is_file = None;

    for path in all_paths {
        let path_is_file = path.is_file();

        // Validate .bri extension for files
        if path_is_file && path.extension().is_none_or(|ext| ext != "bri") {
            anyhow::bail!("file '{}' is not a .bri file", path.display());
        }

        // Prevent mixing files and directories
        match is_file {
            None => is_file = Some(path_is_file),
            Some(expected) if expected != path_is_file => {
                anyhow::bail!("cannot specify both individual files and project directories");
            }
            _ => {}
        }

        let path = std::fs::canonicalize(&path).unwrap_or(path);
        result.insert(path);
    }

    // The unwrap statement is safe here, the number of paths is greater than zero
    // Thus, the value is guaranteed to be Some(_)
    Ok((result, is_file.unwrap()))
}

/// Formats individual `.bri` files directly without loading projects.
async fn format_files(
    reporter: &Reporter,
    files_paths: HashSet<PathBuf>,
    check: bool,
    error_result: &mut Option<()>,
    all_formatted_files: &mut Vec<PathBuf>,
) {
    let result = brioche_core::script::format::format_files(files_paths, check).await;
    match result {
        Ok(formatted) => all_formatted_files.extend(formatted),
        Err(err) => {
            consolidate_result(reporter, None, Err(err), error_result);
        }
    }
}

/// Formats all `.bri` files within projects by loading and processing them.
async fn format_projects(
    reporter: &Reporter,
    project_paths: HashSet<PathBuf>,
    check: bool,
    error_result: &mut Option<()>,
    all_formatted_files: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

    let projects = brioche_core::project::Projects::default();

    let mut projects_to_format = HashSet::with_capacity(project_paths.len());

    // Load each path project
    for project_path in project_paths {
        let project_hash = projects
            .load(
                &brioche,
                &project_path,
                ProjectValidation::Standard,
                ProjectLocking::Unlocked,
            )
            .await;

        let project_hash = match project_hash {
            Ok(project_hash) => project_hash,
            Err(error) => {
                let project_name = format!("project '{}'", project_path.display());
                consolidate_result(reporter, Some(&project_name), Err(error), error_result);
                continue;
            }
        };

        projects_to_format.insert(project_hash);
    }

    if !projects_to_format.is_empty() {
        let result = async {
            if check {
                brioche_core::script::format::check_format(&projects, &projects_to_format).await
            } else {
                brioche_core::script::format::format(&projects, &projects_to_format).await
            }
        }
        .instrument(tracing::info_span!("format"))
        .await;

        match result {
            Ok(formatted) => {
                all_formatted_files.extend(formatted);
            }
            Err(err) => {
                consolidate_result(reporter, None, Err(err), error_result);
            }
        }
    }

    brioche.wait_for_tasks().await;

    Ok(())
}

/// Formats a list of file paths as a bulleted list string.
fn format_file_list(files: &[PathBuf]) -> String {
    use std::fmt::Write as _;

    let mut result = String::new();
    for (i, file) in files.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        write!(result, "- {}", file.display()).unwrap();
    }
    result
}

/// Reports formatting results to the console.
fn report_format_results(reporter: &Reporter, files: &[PathBuf], check: bool) {
    if !check {
        // Format mode: report which files were formatted
        if !files.is_empty() {
            let file_list = format_file_list(files);
            reporter.emit(superconsole::Lines::from_multiline_string(
                &format!("The following files have been formatted:\n{file_list}"),
                superconsole::style::ContentStyle::default(),
            ));
        }
    } else if files.is_empty() {
        // Check mode: all files are formatted
        reporter.emit(superconsole::Lines::from_multiline_string(
            "All files are formatted",
            superconsole::style::ContentStyle::default(),
        ));
    } else {
        // Check mode: some files are not formatted
        let file_list = format_file_list(files);
        reporter.emit(superconsole::Lines::from_multiline_string(
            &format!("The following files are not formatted:\n{file_list}"),
            superconsole::style::ContentStyle::default(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_inputs_defaults_to_current_dir() {
        let (paths, is_file) = partition_paths(vec![], vec![]).unwrap();
        let expected = std::fs::canonicalize(".").unwrap();
        assert_eq!(paths, HashSet::from([expected]));
        assert!(!is_file);
    }

    #[test]
    fn test_deduplicates_paths() {
        let dir = std::env::temp_dir().join("brioche_format_dedup_test");
        std::fs::create_dir_all(&dir).unwrap();

        let path_a = dir.join("./");
        let path_b = dir.clone();
        let (paths, _) = partition_paths(vec![path_a, path_b], vec![]).unwrap();
        assert_eq!(paths.len(), 1);

        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn test_nonexistent_path_accepted() {
        let path = PathBuf::from("this/path/does/not/exist");
        let result = partition_paths(vec![path.clone()], vec![]);
        assert!(result.is_ok());
        let (paths, is_file) = result.unwrap();
        assert!(paths.contains(&path));
        assert!(!is_file);
    }
}
