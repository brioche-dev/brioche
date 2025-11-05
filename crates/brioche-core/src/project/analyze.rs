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

use super::{DependencyDefinition, ProjectDefinition, Version};

#[derive(Debug, Clone)]
pub struct ProjectAnalysis {
    pub definition: super::ProjectDefinition,
    pub local_modules: HashMap<BriocheModuleSpecifier, ModuleAnalysis>,
    pub root_module: BriocheModuleSpecifier,
}

impl ProjectAnalysis {
    #[must_use]
    pub fn dependencies(&self) -> HashMap<String, DependencyDefinition> {
        // Include all dependencies included explicitly in the project definition
        let mut dependencies = self.definition.dependencies.clone();

        // Add all of the dependencies included implicitly from each module
        for module in self.local_modules.values() {
            for import_analysis in module.imports.values() {
                let super::analyze::ImportAnalysis::ExternalProject(dep_name) = import_analysis
                else {
                    // Not an external project import
                    continue;
                };

                let dep_entry = dependencies.entry(dep_name.clone());
                let std::collections::hash_map::Entry::Vacant(dep_entry) = dep_entry else {
                    // Dependency already added
                    continue;
                };

                // Add the dependency, equivalent to using a version of `*`
                dep_entry.insert(DependencyDefinition::Version(Version::Any));
            }
        }

        dependencies
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleAnalysis {
    pub file_id: FileId,
    pub project_subpath: RelativePathBuf,
    pub specifier: BriocheModuleSpecifier,
    pub imports: HashMap<BriocheImportSpecifier, ImportAnalysis>,
    pub statics: BTreeSet<StaticQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportAnalysis {
    ExternalProject(String),
    LocalModule(BriocheModuleSpecifier),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum StaticQuery {
    Include(StaticInclude),
    Glob { patterns: Vec<String> },
    Download { url: url::Url },
    GitRef(GitRefOptions),
}

impl StaticQuery {
    pub fn output_recipe_hash(
        &self,
        output: &StaticOutput,
    ) -> anyhow::Result<Option<crate::recipe::RecipeHash>> {
        let recipe_hash = match output {
            StaticOutput::RecipeHash(hash) => Some(*hash),
            StaticOutput::Kind(StaticOutputKind::Download { hash }) => {
                let Self::Download { url: download_url } = self else {
                    anyhow::bail!("expected download query");
                };
                let recipe = crate::recipe::Recipe::Download(crate::recipe::DownloadRecipe {
                    url: download_url.clone(),
                    hash: hash.clone(),
                });
                Some(recipe.hash())
            }
            StaticOutput::Kind(StaticOutputKind::GitRef { .. }) => {
                // git ref statics don't resolve to a recipe
                None
            }
        };

        Ok(recipe_hash)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct GitRefOptions {
    pub repository: url::Url,

    #[serde(rename = "ref")]
    pub ref_: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum StaticOutput {
    RecipeHash(crate::recipe::RecipeHash),
    Kind(StaticOutputKind),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum StaticOutputKind {
    Download { hash: crate::Hash },
    GitRef { commit: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "include")]
#[serde(rename_all = "snake_case")]
pub enum StaticInclude {
    File { path: String },
    Directory { path: String },
}

impl StaticInclude {
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::File { path } | Self::Directory { path } => path,
        }
    }
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

    let project_definition_json_with_context = project_export.map(|project_export| {
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
        let env = HashMap::default();

        let json = expression_to_json(&project_export_expr, Some(&env))
            .with_context(|| format!("{file_line}: invalid project export"))?;
        anyhow::Ok((json, file_line))
    }).transpose()?;
    let project_definition_json = project_definition_json_with_context
        .clone()
        .map(|(json, _)| json);

    let project_definition = project_definition_json_with_context
        .map(|(json, file_line)| {
            let project_definition: ProjectDefinition = serde_json::from_value(json)
                .with_context(|| format!("{file_line}: invalid project definition"))?;

            anyhow::Ok(project_definition)
        })
        .transpose()?;
    let project_definition = project_definition.unwrap_or_default();

    // Support references to `project` for statics in the root module
    let root_module_env = project_definition_json
        .map(|json| ("project".to_string(), json))
        .into_iter()
        .collect::<HashMap<_, _>>();

    let mut local_modules = HashMap::new();
    let root_module = analyze_module(
        vfs,
        &root_module_path,
        project_path,
        Some(&module),
        &root_module_env,
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
    env: &HashMap<String, serde_json::Value>,
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
    let module = if let Some(module) = module {
        module
    } else {
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

                let env = HashMap::default();

                // Analyze the imported module, but start with a separate
                // environment
                let import_module_specifier = analyze_module(
                    vfs,
                    &import_module_path,
                    project_path,
                    None,
                    &env,
                    local_modules,
                )
                .await?;
                ImportAnalysis::LocalModule(import_module_specifier)
            }
            BriocheImportSpecifier::External(dependency) => {
                ImportAnalysis::ExternalProject(dependency.clone())
            }
        };
        imports.insert(import_specifier, import_analysis);
    }

    let statics =
        find_statics(module, env, display_location).collect::<anyhow::Result<BTreeSet<_>>>()?;

    let local_module = local_modules
        .get_mut(&module_specifier)
        .expect("module not found in local_modules after analyzing imports");
    local_module.imports = imports;
    local_module.statics = statics;

    Ok(module_specifier)
}

pub fn find_imports<'a, D>(
    module: &'a biome_js_syntax::JsModule,
    display_location: impl FnMut(usize) -> D + Clone + 'a,
) -> impl Iterator<Item = anyhow::Result<BriocheImportSpecifier>> + 'a
where
    D: std::fmt::Display,
{
    find_top_level_imports(module, display_location.clone())
        .chain(find_dynamic_imports(module, display_location))
}

fn find_top_level_imports<'a, D>(
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
        .filter_map(std::result::Result::transpose)
}

pub fn find_dynamic_imports<'a, D>(
    module: &'a biome_js_syntax::JsModule,
    mut display_location: impl FnMut(usize) -> D + 'a,
) -> impl Iterator<Item = anyhow::Result<BriocheImportSpecifier>> + 'a
where
    D: std::fmt::Display,
{
    module
        .syntax()
        .descendants()
        .map(move |node| {
            if let Some(import_call_expr) = biome_js_syntax::JsImportCallExpression::cast(node) {
                let location =
                    display_location(import_call_expr.syntax().text_range().start().into());

                // Get the arguments
                let args = import_call_expr
                    .arguments()
                    .with_context(|| format!("{location}: failed to parse import call statement"))?
                    .args();
                let args = args
                    .iter()
                    .map(|arg| arg_to_string_literal(arg, None))
                    .map(|arg| {
                        arg.with_context(|| {
                            format!("{location}: invalid arg to Brioche.includeFile")
                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;

                // Ensure there's exactly one argument
                let specifier = match &args[..] {
                    [specifier] => specifier.clone(),
                    _ => {
                        anyhow::bail!(
                            "{location}: import() is only supported with exactly one argument",
                        );
                    }
                };
                let import_specifier: BriocheImportSpecifier = specifier
                    .parse()
                    .with_context(|| format!("{location}: invalid import specifier"))?;
                Ok(Some(import_specifier))
            } else {
                Ok(None)
            }
        })
        .filter_map(std::result::Result::transpose)
}

pub fn find_statics<'a, D>(
    module: &'a biome_js_syntax::JsModule,
    env: &'a HashMap<String, serde_json::Value>,
    mut display_location: impl FnMut(usize) -> D + 'a,
) -> impl Iterator<Item = anyhow::Result<StaticQuery>> + 'a
where
    D: std::fmt::Display,
{
    module
        .syntax()
        .descendants()
        .map(move |node| {
            // Get a call expression (e.g. `Brioche.includeFile(...)`)
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

            // Filter down to calls to one of the static methods
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

            let location = display_location(call_expr.syntax().text_range().start().into());
            match callee_member_text {
                "includeFile" => {
                    // Get the arguments
                    let args = call_expr.arguments()?.args();
                    let args = args
                        .iter()
                        .map(|arg| arg_to_string_literal(arg, Some(env)))
                        .map(|arg| {
                            arg.with_context(|| {
                                format!("{location}: invalid arg to Brioche.includeFile")
                            })
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    // Ensure there's exactly one argument
                    let path = match &args[..] {
                        [path] => path.clone(),
                        _ => {
                            anyhow::bail!(
                                "{location}: Brioche.includeFile() must take exactly one argument",
                            );
                        }
                    };

                    Ok(Some(StaticQuery::Include(StaticInclude::File {
                        path,
                    })))
                }
                "includeDirectory" => {
                    // Get the arguments
                    let args = call_expr.arguments()?.args();
                    let args = args
                        .iter()
                        .map(|arg| arg_to_string_literal(arg, Some(env)))
                        .map(|arg| {
                            arg.with_context(|| {
                                format!("{location}: invalid arg to Brioche.includeDirectory")
                            })
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    // Ensure there's exactly one argument
                    let path = match &args[..] {
                        [path] => path.clone(),
                        _ => {
                            anyhow::bail!(
                                "{location}: Brioche.includeDirectory() must take exactly one argument",
                            );
                        }
                    };

                    Ok(Some(StaticQuery::Include(StaticInclude::Directory {
                        path,
                    })))
                }
                "glob" => {
                    // Get the arguments
                    let args = call_expr.arguments()?.args();
                    let args = args
                        .iter()
                        .map(|arg| arg_to_string_literal(arg, Some(env)))
                        .map(|arg| {
                            arg.with_context(|| {
                                format!("{location}: invalid arg to Brioche.glob")
                            })
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    Ok(Some(StaticQuery::Glob { patterns: args }))
                }
                "download" => {
                    // Get the arguments
                    let args = call_expr.arguments()?.args();
                    let args = args
                        .iter()
                        .map(|arg| arg_to_string_literal(arg, Some(env)))
                        .map(|arg| {
                            arg.with_context(|| {
                                format!("{location}: invalid arg to Brioche.download")
                            })
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    // Ensure there's exactly one argument
                    let [url] = &args[..] else {
                        anyhow::bail!(
                            "{location}: Brioche.download() must take exactly one argument",
                        );
                    };

                    // Parse the URL
                    let url = url.parse().with_context(|| {
                        format!("{location}: invalid URL for Brioche.download")
                    })?;

                    Ok(Some(StaticQuery::Download { url }))
                }
                "gitRef" | "gitCheckout" => {
                    // Get the arguments
                    let args = call_expr.arguments()?.args();
                    let args = args
                        .iter()
                        .map(|arg| arg_to_json(arg, Some(env)))
                        .map(|arg| {
                            arg.with_context(|| {
                                format!("{location}: invalid arg to Brioche.{callee_member_text}")
                            })
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    // Ensure there's exactly one argument
                    let options = match &args[..] {
                        [options] => options.clone(),
                        _ => {
                            anyhow::bail!(
                                "{location}: Brioche.{callee_member_text}() must take exactly one argument",
                            );
                        }
                    };

                    // Parse the options
                    let options = serde_json::from_value(options).with_context(|| {
                        format!("{location}: invalid options for Brioche.{callee_member_text}, expected an object with the keys `repository` and `ref`")
                    })?;

                    Ok(Some(StaticQuery::GitRef(options)))
                }
                _ => Ok(None),
            }
        })
        .filter_map(std::result::Result::transpose)
}

pub fn expression_to_json(
    expr: &biome_js_syntax::AnyJsExpression,
    env: Option<&HashMap<String, serde_json::Value>>,
) -> anyhow::Result<serde_json::Value> {
    use biome_js_syntax::{AnyJsExpression as Expr, AnyJsLiteralExpression as Literal};
    match expr {
        Expr::AnyJsLiteralExpression(literal) => match literal {
            Literal::JsBooleanLiteralExpression(boolean) => {
                match boolean.value_token()?.text_trimmed() {
                    "true" => Ok(serde_json::Value::Bool(true)),
                    "false" => Ok(serde_json::Value::Bool(false)),
                    other => {
                        anyhow::bail!("invalid boolean literal: {other:?}");
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
                if value.contains('\\') {
                    // TODO: Figure out how to properly unescape the string
                    anyhow::bail!("unsupported escape sequence in string literal");
                }

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
                let biome_js_syntax::AnyJsArrayElement::AnyJsExpression(element) = element else {
                    anyhow::bail!("[{n}]: unsupported array element");
                };
                let element =
                    expression_to_json(&element, env).with_context(|| format!("[{n}]"))?;
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
                                let key = expression_to_json(&key_expr, env)
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
                        let value = expression_to_json(&value, env).with_context(|| key.clone())?;
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
        Expr::JsTemplateExpression(template) => {
            anyhow::ensure!(
                template.tag().is_none(),
                "template literals cannot have tags"
            );

            let components = template
                .elements()
                .iter()
                .map(|element| {
                    let value = match element {
                        biome_js_syntax::AnyJsTemplateElement::JsTemplateChunkElement(chunk) => {
                            let string = chunk.text();

                            anyhow::ensure!(!string.contains('\\'), "unsupported escape sequence");

                            string
                        }
                        biome_js_syntax::AnyJsTemplateElement::JsTemplateElement(element) => {
                            let expr = element
                                .expression()
                                .context("invalid template expression")?;
                            let value = expression_to_json(&expr, env)
                                .with_context(|| "invalid template expression")?;

                            let string = value
                                .as_str()
                                .context("template component must be a string")?;

                            string.to_owned()
                        }
                    };

                    anyhow::Ok(value)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;

            Ok(serde_json::Value::String(components.join("")))
        }
        Expr::JsIdentifierExpression(ident) => {
            let name = ident.name().context("invalid identifier")?;
            let name = name.text();
            let value = env
                .and_then(|env| env.get(&name))
                .with_context(|| format!("variable {name:?} is not allowed in this context"))?;
            Ok(value.clone())
        }
        Expr::JsStaticMemberExpression(expr) => {
            let object = expr.object().context("invalid object reference")?;
            let object = expression_to_json(&object, env).context("invalid object reference")?;
            let object = object.as_object().context("invalid object reference")?;

            let member = expr.member().context("invalid member reference")?;
            let member = member.text();

            let value = object
                .get(&member)
                .with_context(|| format!("member {member:?} not found in object"))?;
            Ok(value.clone())
        }
        Expr::JsComputedMemberExpression(expr) => {
            let object = expr.object().context("invalid object reference")?;
            let object = expression_to_json(&object, env).context("invalid object reference")?;

            let member = expr.member().context("invalid member reference")?;
            let member = expression_to_json(&member, env).context("invalid member reference")?;

            let value = match (object, member) {
                (serde_json::Value::Object(object), serde_json::Value::String(member)) => object
                    .get(&member)
                    .cloned()
                    .with_context(|| format!("member {member:?} not found in object"))?,
                (serde_json::Value::Array(array), serde_json::Value::Number(member)) => {
                    let member = member.as_u64().context("invalid array index")?;
                    let member: usize = member.try_into().context("invalid array index")?;

                    array
                        .get(member)
                        .cloned()
                        .with_context(|| format!("index {member} out of bounds"))?
                }
                _ => {
                    anyhow::bail!("unsupported index expression");
                }
            };

            Ok(value)
        }
        Expr::JsParenthesizedExpression(expr) => {
            let value = expression_to_json(&expr.expression()?, env)?;
            Ok(value)
        }
        Expr::TsAsExpression(expr) => {
            let value = expression_to_json(&expr.expression()?, env)?;
            Ok(value)
        }
        Expr::TsNonNullAssertionExpression(expr) => {
            let value = expression_to_json(&expr.expression()?, env)?;
            Ok(value)
        }
        Expr::TsSatisfiesExpression(expr) => {
            let value = expression_to_json(&expr.expression()?, env)?;
            Ok(value)
        }
        Expr::TsTypeAssertionExpression(expr) => {
            let value = expression_to_json(&expr.expression()?, env)?;
            Ok(value)
        }
        _ => {
            anyhow::bail!("unsupported expression");
        }
    }
}

fn arg_to_json(
    arg: biome_rowan::SyntaxResult<biome_js_syntax::AnyJsCallArgument>,
    env: Option<&HashMap<String, serde_json::Value>>,
) -> anyhow::Result<serde_json::Value> {
    let arg = arg?;
    let arg = arg
        .as_any_js_expression()
        .context("spread arguments are not supported")?;
    let arg = expression_to_json(arg, env)?;

    anyhow::Ok(arg)
}

fn arg_to_string_literal(
    arg: biome_rowan::SyntaxResult<biome_js_syntax::AnyJsCallArgument>,
    env: Option<&HashMap<String, serde_json::Value>>,
) -> anyhow::Result<String> {
    let arg = arg_to_json(arg, env)?;
    let arg = arg.as_str().context("expected string argument")?;

    anyhow::Ok(arg.to_string())
}
