use std::path::PathBuf;

use crate::project::{ProjectHash, Projects};

/// Formats the specified project using the provided formatter.
///
/// This function takes a reference to the `Projects` struct, a `ProjectHash` representing the project to format.
/// It returns a `Result` containing a vector of `PathBuf` representing the paths of the formatted files,
/// or an `anyhow::Error` if an error occurs.
pub async fn format(
    projects: &Projects,
    project_hash: ProjectHash,
) -> anyhow::Result<Vec<PathBuf>> {
    format_project(projects, project_hash, false).await
}

/// Checks the formatting of the specified project using the provided formatter.
///
/// This function takes a reference to the `Projects` struct, a `ProjectHash` representing the project to check.
/// It returns a `Result` containing a vector of `PathBuf` representing the paths of the unformatted files,
/// or an `anyhow::Error` if an error occurs.
pub async fn check_format(
    projects: &Projects,
    project_hash: ProjectHash,
) -> anyhow::Result<Vec<PathBuf>> {
    format_project(projects, project_hash, true).await
}

#[tracing::instrument(skip(projects), err)]
async fn format_project(
    projects: &Projects,
    project_hash: ProjectHash,
    check: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut result = vec![];

    let module_paths = projects.project_module_paths(project_hash)?;
    for path in module_paths {
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

/// Formats the code using the specified formatter.
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
