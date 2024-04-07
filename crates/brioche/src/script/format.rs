use std::path::PathBuf;

use crate::project::{ProjectHash, Projects};

#[tracing::instrument(skip(projects), err)]
pub async fn format(projects: &Projects, project_hash: ProjectHash) -> anyhow::Result<()> {
    let module_paths = projects.project_module_paths(project_hash)?;
    for path in &module_paths {
        let contents = tokio::fs::read_to_string(path).await?;

        let formatted_contents = format_code(&contents)?;

        tokio::fs::write(path, &formatted_contents).await?;
    }

    Ok(())
}

#[tracing::instrument(skip(projects), err)]
pub async fn check_format(
    projects: &Projects,
    project_hash: ProjectHash,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut unformatted = vec![];

    let module_paths = projects.project_module_paths(project_hash)?;
    for path in &module_paths {
        let contents = tokio::fs::read_to_string(path).await?;

        let formatted_contents = format_code(&contents)?;

        if contents != formatted_contents {
            unformatted.push(path.to_owned());
        }
    }

    Ok(unformatted)
}

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
