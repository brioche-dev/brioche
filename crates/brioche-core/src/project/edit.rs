use std::path::Path;

use anyhow::Context as _;
use biome_rowan::{AstNode as _, AstNodeExt as _, AstNodeList as _, AstSeparatedList as _};

use crate::vfs::Vfs;

#[derive(Debug, Default, Clone)]
pub struct ProjectChanges {
    pub project_definition: Option<serde_json::Value>,
}

pub async fn edit_project(
    vfs: &Vfs,
    project_path: &Path,
    changes: ProjectChanges,
) -> anyhow::Result<bool> {
    let root_module_path = project_path.join("project.bri");
    let file = root_module_path.display();
    let (_, contents) = vfs
        .load(&root_module_path)
        .await
        .with_context(|| format!("{file}: failed to read root module file"))?;
    let contents =
        std::str::from_utf8(&contents).with_context(|| format!("{file}: invalid UTF-8"))?;
    let parsed = biome_js_parser::parse(
        contents,
        biome_js_syntax::JsFileSource::ts().with_module_kind(biome_js_syntax::ModuleKind::Module),
        biome_js_parser::JsParserOptions::default(),
    )
    .cast::<biome_js_syntax::JsModule>()
    .expect("failed to cast module");

    let mut module = parsed
        .try_tree()
        .with_context(|| format!("{file}: failed to parse module"))?;
    let mut did_update = false;

    let project_export = module.items().iter().find_map(|item| {
        let export = item.as_js_export()?;

        let export_clause = export.export_clause().ok()?;
        let declaration = export_clause.as_any_js_declaration_clause()?;
        let var_declaration = declaration.as_js_variable_declaration_clause()?;
        let var_declaration = var_declaration.declaration().ok()?;

        var_declaration.declarators().iter().find_map(|declarator| {
            let declarator = declarator.ok()?;
            let id = declarator.id().ok()?;
            let id = id.as_any_js_binding()?.as_js_identifier_binding()?;
            let id_name = id.name_token().ok()?;

            if id_name.text_trimmed() == "project" {
                Some(declarator)
            } else {
                None
            }
        })
    });

    if let Some(new_project_definition) = &changes.project_definition {
        let Some(project_export) = project_export else {
            anyhow::bail!("project does not have a project definition");
        };

        let line = contents[..project_export.syntax().text_range().start().into()]
            .lines()
            .count();
        let file_line = format!("{file}:{line}");
        let project_export_expr = project_export
            .initializer()
            .map(|init| {
                let expr = init.expression().with_context(|| {
                    format!("{file_line}: invalid project export: failed to parse expression")
                })?;
                anyhow::Ok(expr)
            })
            .with_context(|| format!("{file_line}: invalid project export: expected assignment like `export const project = {{ ... }}`"))??;
        let current_project_export_json =
            super::analyze::expression_to_json(&project_export_expr, &Default::default())
                .with_context(|| format!("{file_line}: invalid project export"))?;

        if current_project_export_json != *new_project_definition {
            let new_project_export_expr = json_to_expression(new_project_definition);

            module = module
                .replace_node(project_export_expr, new_project_export_expr)
                .expect("failed to update JS AST node");
            did_update = true;
        }
    }

    if did_update {
        let new_contents = crate::script::format::format_code(&module.text())?;

        tokio::fs::write(&root_module_path, &new_contents[..]).await?;
        vfs.unload(&root_module_path)?;
        vfs.load(&root_module_path).await?;
    }

    Ok(did_update)
}

fn json_to_expression(value: &serde_json::Value) -> biome_js_syntax::AnyJsExpression {
    // Generate a dummy JavaScript module with the JSON value embedded.
    // biome_js_parser / biome_syntax don't seem to offer a way to construct
    // AST nodes publicly, so generating valid JS source and parsing it
    // seems like the best option
    let json = serde_json::to_string_pretty(value).expect("failed to serialize JSON");
    let script = format!("export const project = {json};");

    // Parse the module
    let parsed = biome_js_parser::parse(
        &script,
        biome_js_syntax::JsFileSource::ts().with_module_kind(biome_js_syntax::ModuleKind::Module),
        biome_js_parser::JsParserOptions::default(),
    )
    .cast::<biome_js_syntax::JsModule>()
    .expect("failed to cast module");

    // Extract the expression
    let module = parsed.try_tree().expect("failed to parse module");
    let project_export = module
        .items()
        .iter()
        .find_map(|item| {
            let export = item.as_js_export()?;

            let export_clause = export.export_clause().ok()?;
            let declaration = export_clause.as_any_js_declaration_clause()?;
            let var_declaration = declaration.as_js_variable_declaration_clause()?;
            let var_declaration = var_declaration.declaration().ok()?;

            var_declaration.declarators().iter().find_map(|declarator| {
                let declarator = declarator.ok()?;
                let id = declarator.id().ok()?;
                let id = id.as_any_js_binding()?.as_js_identifier_binding()?;
                let id_name = id.name_token().ok()?;

                if id_name.text_trimmed() == "project" {
                    Some(declarator)
                } else {
                    None
                }
            })
        })
        .expect("failed to find project export");
    let project_export_initializer = project_export
        .initializer()
        .expect("failed to get project export initializer");
    project_export_initializer
        .expression()
        .expect("failed to parse project export expression")
}
