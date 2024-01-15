use crate::brioche::project::Project;

#[tracing::instrument(skip(project), err)]
pub async fn format(project: &Project) -> anyhow::Result<()> {
    for path in project.local_module_paths() {
        let contents = tokio::fs::read_to_string(path).await?;

        let formatted_contents = format_code(&contents)?;

        tokio::fs::write(path, &formatted_contents).await?;
    }

    Ok(())
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
