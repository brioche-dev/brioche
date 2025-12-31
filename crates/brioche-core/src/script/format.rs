use std::collections::HashSet;
use std::path::PathBuf;

use crate::project::{ProjectHash, Projects};

/// Formats the specified projects.
///
/// This function takes a reference to the `Projects` struct, a list of `ProjectHash` representing the projects to format.
/// It returns a `Result` containing a vector of `PathBuf` representing the paths of the formatted files,
/// or an `anyhow::Error` if an error occurs.
pub async fn format(
    projects: &Projects,
    project_hashes: &HashSet<ProjectHash>,
) -> anyhow::Result<Vec<PathBuf>> {
    format_project(projects, project_hashes, false).await
}

/// Checks the formatting of the specified projects.
///
/// This function takes a reference to the `Projects` struct, a list of `ProjectHash` representing the projects to check.
/// It returns a `Result` containing a vector of `PathBuf` representing the paths of the unformatted files,
/// or an `anyhow::Error` if an error occurs.
pub async fn check_format(
    projects: &Projects,
    project_hashes: &HashSet<ProjectHash>,
) -> anyhow::Result<Vec<PathBuf>> {
    format_project(projects, project_hashes, true).await
}

/// Formats a project.
///
/// This function takes a project and formats it.
/// If `check` is true, it only checks formatting without writing changes.
/// It returns a `Result` containing a vector of `PathBuf` representing the paths of files
/// that were formatted (or would be formatted in check mode).
async fn format_project(
    projects: &Projects,
    project_hashes: &HashSet<ProjectHash>,
    check: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let module_paths = projects.project_module_paths_for_projects(project_hashes)?;

    format_files(module_paths, check).await
}

/// Formats individual files.
///
/// This function takes a slice of file paths and formats each one.
/// If `check` is true, it only checks formatting without writing changes.
/// It returns a `Result` containing a vector of `PathBuf` representing the paths of files
/// that were formatted (or would be formatted in check mode).
#[tracing::instrument(skip(paths), err)]
pub async fn format_files(
    paths: impl IntoIterator<Item = PathBuf>,
    check: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let paths = paths.into_iter();

    // Pre-allocate capacity for the result vector based on the worst-case scenario
    let mut result = Vec::with_capacity(paths.size_hint().0);

    for path in paths {
        let contents = tokio::fs::read_to_string(&path).await?;
        let formatted_contents = format_code(&contents)?;

        // Check if the file was formatted
        if contents != formatted_contents {
            if !check {
                tokio::fs::write(&path, &formatted_contents).await?;
            }

            result.push(path);
        }
    }

    Ok(result)
}

/// Formats the code.
///
/// This function takes a string slice `contents` as input and formats the code using the `biome_js_formatter` crate.
/// It returns a `Result` containing the formatted code as a `String`, or an `anyhow::Error` if an error occurs.
pub fn format_code(contents: &str) -> anyhow::Result<String> {
    let file_source =
        biome_js_syntax::JsFileSource::ts().with_module_kind(biome_js_syntax::ModuleKind::Module);
    let parsed = biome_js_parser::parse(
        contents,
        file_source,
        biome_js_parser::JsParserOptions::default(),
    );

    let formatted = biome_js_formatter::format_node(
        biome_js_formatter::context::JsFormatOptions::new(file_source)
            .with_indent_style(biome_formatter::IndentStyle::Space),
        &parsed.syntax(),
    )?;

    let formatted_code = formatted.print()?.into_code();
    Ok(formatted_code)
}
