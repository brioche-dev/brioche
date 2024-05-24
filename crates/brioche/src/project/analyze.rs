use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

use anyhow::Context as _;
use biome_rowan::{AstNode as _, AstNodeList as _, AstSeparatedList as _};
use relative_path::{PathExt as _, RelativePathBuf};

use crate::{
    script::specifier::{BriocheImportSpecifier, BriocheModuleSpecifier},
    vfs::{FileId, Vfs},
};

use super::ProjectDefinition;

#[derive(Debug, Clone)]
pub struct ProjectAnalysis {
    pub definition: super::ProjectDefinition,
    pub local_modules: HashMap<BriocheModuleSpecifier, ModuleAnalysis>,
    pub root_module: BriocheModuleSpecifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleAnalysis {
    pub file_id: FileId,
    pub project_subpath: RelativePathBuf,
    pub specifier: BriocheModuleSpecifier,
    pub imports: HashMap<BriocheImportSpecifier, ImportAnalysis>,
    pub statics: BTreeSet<StaticAnalysis>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportAnalysis {
    ExternalProject(String),
    LocalModule(BriocheModuleSpecifier),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum StaticAnalysis {
    Get { path: String },
}

pub async fn analyze_project(vfs: &Vfs, project_path: &Path) -> anyhow::Result<ProjectAnalysis> {
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

    let project_definition = project_export.map(|project_export| {
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
        let project_definition: ProjectDefinition = serde_json::from_value(json)
            .with_context(|| format!("{file_line}: invalid project definition"))?;

        anyhow::Ok(project_definition)
    }).transpose()?;
    let project_definition = project_definition.unwrap_or_default();

    let mut local_modules = HashMap::new();
    let root_module = analyze_module(
        vfs,
        &root_module_path,
        project_path,
        Some(&module),
        &mut local_modules,
    )
    .await?;

    Ok(ProjectAnalysis {
        definition: project_definition,
        local_modules,
        root_module,
    })
}

#[async_recursion::async_recursion(?Send)]
pub async fn analyze_module(
    vfs: &Vfs,
    module_path: &Path,
    project_path: &Path,
    module: Option<&'async_recursion biome_js_syntax::JsModule>,
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

    let display_path = module_path.display();

    let contents = match local_modules.entry(module_specifier.clone()) {
        std::collections::hash_map::Entry::Occupied(_) => {
            // We've already analyzed this module (or we're still analyzing it)
            return Ok(module_specifier);
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            let project_subpath = module_path.relative_to(project_path).with_context(|| {
                format!(
                    "{display_path}: failed to resolve relative to project root {}",
                    project_path.display()
                )
            })?;
            let project_subpath = project_subpath.normalize();
            anyhow::ensure!(
                crate::fs_utils::is_subpath(&project_subpath),
                "{display_path}: module escapes project root"
            );

            let (file_id, contents) = vfs
                .load(&module_path)
                .await
                .with_context(|| format!("{display_path}: failed to read file"))?;

            // We haven't analyzed this module yet. When recursing, we don't
            // want to re-analyze this module again, so insert a placeholder
            // that we'll update later.
            entry.insert(ModuleAnalysis {
                file_id,
                project_subpath,
                specifier: module_specifier.clone(),
                imports: HashMap::new(),
                statics: BTreeSet::new(),
            });

            contents
        }
    };

    let contents =
        std::str::from_utf8(&contents).with_context(|| format!("{display_path}: invalid UTF-8"))?;

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
                .with_context(|| format!("{display_path}: failed to parse module"))?;
            &parsed_module
        }
    };

    let display_location = |offset| {
        let line = contents[..offset].lines().count();
        format!("{module_specifier}:{line}")
    };

    let mut imports = HashMap::new();

    let import_specifiers = find_imports(module, display_location);
    for import_specifier in import_specifiers {
        let import_specifier = import_specifier?;

        let import_analysis = match &import_specifier {
            BriocheImportSpecifier::Local(local_import) => {
                let import_module_path = match local_import {
                    crate::script::specifier::BriocheLocalImportSpecifier::Relative(subpath) => {
                        module_path
                            .parent()
                            .context("invalid module path")?
                            .join(subpath)
                    }
                    crate::script::specifier::BriocheLocalImportSpecifier::ProjectRoot(subpath) => {
                        project_path.join(subpath)
                    }
                };
                anyhow::ensure!(
                    import_module_path.starts_with(project_path),
                    "invalid import path: must be within project root",
                );
                let import_module_specifier =
                    analyze_module(vfs, &import_module_path, project_path, None, local_modules)
                        .await?;
                ImportAnalysis::LocalModule(import_module_specifier)
            }
            BriocheImportSpecifier::External(dependency) => {
                ImportAnalysis::ExternalProject(dependency.to_string())
            }
        };
        imports.insert(import_specifier, import_analysis);
    }

    let statics =
        find_statics(module, display_location).collect::<anyhow::Result<BTreeSet<_>>>()?;

    let local_module = local_modules
        .get_mut(&module_specifier)
        .expect("module not found in local_modules after analyzing imports");
    local_module.imports = imports;
    local_module.statics = statics;

    Ok(module_specifier)
}

pub fn find_imports<'a, D>(
    module: &'a biome_js_syntax::JsModule,
    mut display_location: impl FnMut(usize) -> D + 'a,
) -> impl Iterator<Item = anyhow::Result<BriocheImportSpecifier>> + 'a
where
    D: std::fmt::Display,
{
    module
        .items()
        .iter()
        .map(move |item| {
            let location = display_location(item.syntax().text_range().start().into());
            let import_source = match item {
                biome_js_syntax::AnyJsModuleItem::JsExport(export_item) => {
                    let export_clause = export_item
                        .export_clause()
                        .with_context(|| format!("{location}: failed to parse export statement"))?;
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
                            return Ok(None);
                        }
                    }
                }
                biome_js_syntax::AnyJsModuleItem::JsImport(import_item) => {
                    let import_clause = import_item
                        .import_clause()
                        .with_context(|| format!("{location}: failed to parse import statement"))?;
                    import_clause.source()
                }
                biome_js_syntax::AnyJsModuleItem::AnyJsStatement(_) => {
                    // Not an import or export statement
                    return Ok(None);
                }
            };

            let import_source = import_source
                .with_context(|| format!("{location}: failed to parse import source"))?;
            let import_source = import_source
                .inner_string_text()
                .with_context(|| format!("{location}: failed to get import source text"))?;
            let import_source = import_source.text();
            let import_specifier: BriocheImportSpecifier = import_source
                .parse()
                .with_context(|| format!("{location}: invalid import specifier"))?;
            Ok(Some(import_specifier))
        })
        .filter_map(|result| result.transpose())
}

pub fn find_statics<'a, D>(
    module: &'a biome_js_syntax::JsModule,
    mut display_location: impl FnMut(usize) -> D + 'a,
) -> impl Iterator<Item = anyhow::Result<StaticAnalysis>> + 'a
where
    D: std::fmt::Display,
{
    module
        .syntax()
        .descendants()
        .map(move |node| {
            // Get a call expression (e.g. `Brioche.get(...)`)
            let Some(call_expr) = biome_js_syntax::JsCallExpression::cast(node) else {
                return Ok(None);
            };

            let Ok(callee) = call_expr.callee() else {
                return Ok(None);
            };
            let Some(callee) = callee.as_js_static_member_expression() else {
                return Ok(None);
            };

            // Filter down to call expressions accessing a member from `Brioche`
            let Ok(callee_object) = callee.object() else {
                return Ok(None);
            };
            let Some(callee_object) = callee_object.as_js_identifier_expression() else {
                return Ok(None);
            };
            let Ok(callee_object_name) = callee_object.name() else {
                return Ok(None);
            };
            if !callee_object_name.has_name("Brioche") {
                return Ok(None);
            }

            // Filter down to calls to the `.get()` method
            let Ok(callee_member) = callee.member() else {
                return Ok(None);
            };
            let Some(callee_member) = callee_member.as_js_name() else {
                return Ok(None);
            };
            let Ok(callee_member_text) = callee_member.value_token() else {
                return Ok(None);
            };
            let callee_member_text = callee_member_text.text_trimmed();

            if callee_member_text != "get" {
                return Ok(None);
            }

            let location = display_location(call_expr.syntax().text_range().start().into());

            // Get the arguments
            let args = call_expr.arguments()?.args();
            let args = args
                .iter()
                .map(|arg| {
                    let arg = arg?;
                    let arg = arg
                        .as_any_js_expression()
                        .context("spread arguments are not supported")?;
                    let arg = arg
                        .as_any_js_literal_expression()
                        .context("argument must be a string literal")?;
                    let arg = arg
                        .as_js_string_literal_expression()
                        .context("argument must be a string literal")?;
                    let arg = arg
                        .inner_string_text()
                        .context("invalid string literal argument")?;

                    anyhow::Ok(arg)
                })
                .map(|arg| arg.with_context(|| format!("{location}: invalid arg to Brioche.get")))
                .collect::<anyhow::Result<Vec<_>>>()?;

            let get_path = match &*args {
                [path] => path.text(),
                _ => {
                    anyhow::bail!("{location}: Brioche.get() must take exactly one argument",);
                }
            };

            Ok(Some(StaticAnalysis::Get {
                path: get_path.to_string(),
            }))
        })
        .filter_map(|result| result.transpose())
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
