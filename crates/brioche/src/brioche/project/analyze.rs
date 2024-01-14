use std::{collections::HashMap, path::Path};

use anyhow::Context as _;
use biome_rowan::{AstNode as _, AstNodeList as _, AstSeparatedList as _};

use crate::brioche::script::specifier::{BriocheImportSpecifier, BriocheModuleSpecifier};

#[derive(Debug)]
pub struct ProjectAnalysis {
    pub definition: super::ProjectDefinition,
    pub local_modules: HashMap<BriocheModuleSpecifier, ModuleAnalysis>,
    pub root_module: BriocheModuleSpecifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleAnalysis {
    pub specifier: BriocheModuleSpecifier,
    pub imports: HashMap<BriocheImportSpecifier, ImportAnalysis>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportAnalysis {
    ExternalProject(String),
    LocalModule(BriocheModuleSpecifier),
}

// TODO: Make async
pub fn analyze_project(project_path: &Path) -> anyhow::Result<ProjectAnalysis> {
    let root_module_path = project_path.join("project.bri");
    let file = root_module_path.display();
    let contents = std::fs::read_to_string(&root_module_path)
        .with_context(|| format!("{file}: Failed to read root module file"))?;
    let parsed = biome_js_parser::parse(
        &contents,
        biome_js_syntax::JsFileSource::ts().with_module_kind(biome_js_syntax::ModuleKind::Module),
        biome_js_parser::JsParserOptions::default(),
    )
    .cast::<biome_js_syntax::JsModule>()
    .expect("failed to cast module");

    let module = parsed
        .try_tree()
        .with_context(|| format!("{file}: failed to parse module"))?;

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

    let project_export = project_export
        .with_context(|| format!("{file}: expected a top-level export named `project`"))?;
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
            anyhow::Ok(expr.clone())
        })
        .with_context(|| format!("{file_line}: invalid project export: expected assignment like `export const project = {{ ... }}`"))??;

    let json = expression_to_json(&project_export_expr)
        .with_context(|| format!("{file_line}: invalid project export"))?;
    let project_definition = serde_json::from_value(json)
        .with_context(|| format!("{file_line}: invalid project definition"))?;

    let mut local_modules = HashMap::new();
    let root_module = analyze_module(
        &root_module_path,
        project_path,
        Some(&contents),
        Some(&module),
        &mut local_modules,
    )?;

    Ok(ProjectAnalysis {
        definition: project_definition,
        local_modules,
        root_module,
    })
}

// TODO: Make async
pub fn analyze_module(
    module_path: &Path,
    project_path: &Path,
    contents: Option<&str>,
    module: Option<&biome_js_syntax::JsModule>,
    local_modules: &mut HashMap<BriocheModuleSpecifier, ModuleAnalysis>,
) -> anyhow::Result<BriocheModuleSpecifier> {
    let module_path = if module_path == project_path {
        module_path.join("project.bri")
    } else if module_path.is_dir() {
        module_path.join("index.bri")
    } else {
        module_path.to_owned()
    };

    let module_specifier = BriocheModuleSpecifier::File {
        path: module_path.clone(),
    };
    match local_modules.entry(module_specifier.clone()) {
        std::collections::hash_map::Entry::Occupied(_) => {
            // We've already analyzed this module (or we're still analyzing it)
            return Ok(module_specifier);
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            // We haven't analyzed this module yet. When recursing, we don't
            // want to re-analyze this module again, so insert a placeholder
            // that we'll update later.
            entry.insert(ModuleAnalysis {
                specifier: module_specifier.clone(),
                imports: HashMap::new(),
            });
        }
    };

    let file = module_path.display();

    let read_contents;
    let contents = match contents {
        Some(contents) => contents,
        None => {
            read_contents = std::fs::read_to_string(&module_path)
                .with_context(|| format!("{file}: Failed to read file"))?;
            &read_contents
        }
    };

    let parsed_module;
    let module = match module {
        Some(module) => module,
        None => {
            let parsed = biome_js_parser::parse(
                contents,
                biome_js_syntax::JsFileSource::ts()
                    .with_module_kind(biome_js_syntax::ModuleKind::Module),
                biome_js_parser::JsParserOptions::default(),
            )
            .cast::<biome_js_syntax::JsModule>()
            .expect("failed to cast module");

            parsed_module = parsed
                .try_tree()
                .with_context(|| format!("{file}: failed to parse module"))?;
            &parsed_module
        }
    };

    let mut imports = HashMap::new();

    for module_item in module.items() {
        let line = contents[..module_item.syntax().text_range().start().into()]
            .lines()
            .count();

        let import_source = match module_item {
            biome_js_syntax::AnyJsModuleItem::JsExport(export_item) => {
                let export_clause = export_item
                    .export_clause()
                    .with_context(|| format!("{file}:{line}: failed to parse export statement"))?;
                match export_clause {
                    biome_js_syntax::AnyJsExportClause::JsExportFromClause(clause) => {
                        clause.source()
                    }
                    biome_js_syntax::AnyJsExportClause::JsExportNamedFromClause(clause) => {
                        clause.source()
                    }
                    biome_js_syntax::AnyJsExportClause::AnyJsDeclarationClause(_)
                    | biome_js_syntax::AnyJsExportClause::JsExportDefaultDeclarationClause(_)
                    | biome_js_syntax::AnyJsExportClause::JsExportDefaultExpressionClause(_)
                    | biome_js_syntax::AnyJsExportClause::JsExportNamedClause(_)
                    | biome_js_syntax::AnyJsExportClause::TsExportAsNamespaceClause(_)
                    | biome_js_syntax::AnyJsExportClause::TsExportAssignmentClause(_)
                    | biome_js_syntax::AnyJsExportClause::TsExportDeclareClause(_) => {
                        // Not an export from another module
                        continue;
                    }
                }
            }
            biome_js_syntax::AnyJsModuleItem::JsImport(import_item) => {
                let import_clause = import_item
                    .import_clause()
                    .with_context(|| format!("{file}:{line}: failed to parse import statement"))?;
                import_clause.source()
            }
            biome_js_syntax::AnyJsModuleItem::AnyJsStatement(_) => {
                // Not an import or export statement
                continue;
            }
        };

        let import_source = import_source
            .with_context(|| format!("{file}:{line}: failed to parse import source"))?;
        let import_source = import_source
            .inner_string_text()
            .with_context(|| format!("{file}:{line}: failed to get import source text"))?;
        let import_source = import_source.text();
        let import_specifier: BriocheImportSpecifier = import_source
            .parse()
            .with_context(|| format!("{file}:{line}: invalid import specifier"))?;

        let import_analysis = match &import_specifier {
            BriocheImportSpecifier::Local(local_import) => {
                let import_module_path = match local_import {
                    crate::brioche::script::specifier::BriocheLocalImportSpecifier::Relative(
                        subpath,
                    ) => module_path
                        .parent()
                        .context("invalid module path")?
                        .join(subpath),
                    crate::brioche::script::specifier::BriocheLocalImportSpecifier::ProjectRoot(
                        subpath,
                    ) => project_path.join(subpath),
                };
                anyhow::ensure!(
                    import_module_path.starts_with(project_path),
                    "invalid import path: must be within project root",
                );
                let import_module_specifier =
                    analyze_module(&import_module_path, project_path, None, None, local_modules)?;
                ImportAnalysis::LocalModule(import_module_specifier)
            }
            BriocheImportSpecifier::External(dependency) => {
                ImportAnalysis::ExternalProject(dependency.to_string())
            }
        };
        imports.insert(import_specifier, import_analysis);
    }

    let local_module = local_modules
        .get_mut(&module_specifier)
        .expect("module not found in local_modules after analyzing imports");
    local_module.imports = imports;

    Ok(module_specifier)
}

fn expression_to_json(
    expr: &biome_js_syntax::AnyJsExpression,
) -> anyhow::Result<serde_json::Value> {
    use biome_js_syntax::{AnyJsExpression as Expr, AnyJsLiteralExpression as Literal};
    match expr {
        Expr::AnyJsLiteralExpression(literal) => match literal {
            Literal::JsBooleanLiteralExpression(boolean) => {
                match boolean.value_token()?.text_trimmed() {
                    "true" => Ok(serde_json::Value::Bool(true)),
                    "false" => Ok(serde_json::Value::Bool(false)),
                    other => {
                        anyhow::bail!("invalid boolean literal: {:?}", other);
                    }
                }
            }
            Literal::JsNullLiteralExpression(_) => Ok(serde_json::Value::Null),
            Literal::JsNumberLiteralExpression(number) => {
                let value = number.as_number().context("invalid number")?;
                let value = serde_json::Number::from_f64(value).context("invalid number")?;
                Ok(serde_json::Value::Number(value))
            }
            Literal::JsStringLiteralExpression(string) => {
                let value = string.inner_string_text().context("invalid string")?;
                Ok(serde_json::Value::String(value.text().to_string()))
            }
            _ => {
                anyhow::bail!("invalid literal value");
            }
        },
        Expr::JsArrayExpression(array) => {
            let mut values = Vec::new();
            for (n, element) in array.elements().iter().enumerate() {
                let element = element.with_context(|| format!("[{n}]: syntax error"))?;
                let element = match element {
                    biome_js_syntax::AnyJsArrayElement::AnyJsExpression(expression) => expression,
                    _ => {
                        anyhow::bail!("[{n}]: unsupported array element");
                    }
                };
                let element = expression_to_json(&element).with_context(|| format!("[{n}]"))?;
                values.push(element);
            }
            Ok(serde_json::Value::Array(values))
        }
        Expr::JsObjectExpression(object) => {
            let mut values = serde_json::Map::new();
            for member in object.members() {
                let member = member.with_context(|| "syntax error".to_string())?;
                let (key, value) = match member {
                    biome_js_syntax::AnyJsObjectMember::JsPropertyObjectMember(member) => {
                        let key = member.name().context("invalid object member name")?;
                        let key = match key {
                            biome_js_syntax::AnyJsObjectMemberName::JsComputedMemberName(
                                computed,
                            ) => {
                                let key_expr = computed
                                    .expression()
                                    .context("invalid object member name")?;
                                let key = expression_to_json(&key_expr)
                                    .context("invalid object member name")?;
                                let serde_json::Value::String(key) = key else {
                                    anyhow::bail!("object member name must be a string");
                                };

                                key
                            }
                            biome_js_syntax::AnyJsObjectMemberName::JsLiteralMemberName(name) => {
                                let key = name.name().context("invalid object member name")?;
                                key.text().to_string()
                            }
                        };

                        let value = member
                            .value()
                            .with_context(|| format!("{key}: syntax error"))?;
                        let value = expression_to_json(&value).with_context(|| key.to_string())?;
                        (key, value)
                    }
                    biome_js_syntax::AnyJsObjectMember::JsShorthandPropertyObjectMember(member) => {
                        let key = member.name().context("invalid object member name")?;
                        let key = key.name().context("invalid object member name")?;
                        let key = key.text();
                        anyhow::bail!("{key}: shorthand properties are not supported");
                    }
                    _ => {
                        anyhow::bail!("unsupported object member");
                    }
                };
                values.insert(key, value);
            }

            Ok(serde_json::Value::Object(values))
        }
        Expr::JsParenthesizedExpression(_) => todo!(),
        Expr::TsAsExpression(_) => todo!(),
        Expr::TsNonNullAssertionExpression(_) => todo!(),
        Expr::TsSatisfiesExpression(_) => todo!(),
        Expr::TsTypeAssertionExpression(_) => todo!(),
        _ => {
            anyhow::bail!("unsupported expression");
        }
    }
}
