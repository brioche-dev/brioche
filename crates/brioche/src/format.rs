use std::collections::HashSet;
use std::{path::PathBuf, process::ExitCode};

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

    let (paths, is_file) = partition_paths(args.paths, args.project)?;

    let mut error_result = Option::None;
    // Heuristic pre-allocation: assuming all the paths won't be re-formatted
    // and one quarter of them are going to be updated
    let mut all_formatted_files = Vec::with_capacity(paths.len() / 4);

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

    guard.shutdown_console().await;

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
        return Ok((HashSet::from([PathBuf::from(".")]), false));
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
