use std::{
    cell::RefCell,
    collections::{hash_map::Entry, HashMap},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use anyhow::Context as _;
use deno_core::OpState;
use futures::{StreamExt as _, TryStreamExt as _};
use joinery::JoinableIterator as _;
use specifier::BriocheModuleSpecifier;

use crate::{
    bake::BakeScope,
    project::analyze::{StaticInclude, StaticQuery},
};

use super::{
    blob::BlobHash,
    project::Projects,
    recipe::{Artifact, Recipe, WithMeta},
    script::specifier::BriocheImportSpecifier,
    Brioche,
};

pub mod check;
mod compiler_host;
pub mod evaluate;
pub mod format;
mod js;
pub mod lsp;
pub mod specifier;

#[derive(Clone)]
pub struct ModuleLoaderTask {
    tx: tokio::sync::mpsc::UnboundedSender<ModuleLoaderMessage>,
}

impl ModuleLoaderTask {
    pub fn new(brioche: Brioche, projects: Projects) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        tokio::task::spawn(async move {
            while let Some(message) = rx.recv().await {
                match message {
                    ModuleLoaderMessage::ReadSpecifierContents {
                        specifier,
                        contents_tx,
                    } => {
                        let contents = specifier::read_specifier_contents(&brioche.vfs, &specifier);
                        let _ = contents_tx.send(contents);
                    }
                    ModuleLoaderMessage::ResolveSpecifier {
                        specifier,
                        referrer,
                        resolved_tx,
                    } => {
                        let resolved = specifier::resolve(&projects, &specifier, &referrer);
                        let _ = resolved_tx.send(resolved);
                    }
                    ModuleLoaderMessage::ProjectRootForModulePath {
                        module_path,
                        project_path_tx,
                    } => {
                        let project_hash = projects.find_containing_project(&module_path);
                        let project_hash = match project_hash {
                            Ok(Some(project_hash)) => project_hash,
                            Ok(None) => {
                                let error = anyhow::anyhow!("containing project not found");
                                let _ = project_path_tx.send(Err(error));
                                continue;
                            }
                            Err(error) => {
                                let _ = project_path_tx
                                    .send(Err(error).context("failed to get project path"));
                                continue;
                            }
                        };
                        let project_path = projects.project_root(project_hash)?.clone();
                        let _ = project_path_tx.send(Ok(project_path));
                    }
                }
            }

            anyhow::Ok(())
        });

        Self { tx }
    }
}

#[derive(Clone)]
struct BriocheModuleLoader {
    pub task: ModuleLoaderTask,
    pub sources: Rc<RefCell<HashMap<BriocheModuleSpecifier, ModuleSource>>>,
}

impl BriocheModuleLoader {
    fn new(task: ModuleLoaderTask) -> Self {
        Self {
            task,
            sources: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn load_module_source(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
    ) -> Result<deno_core::ModuleSource, anyhow::Error> {
        let brioche_module_specifier: BriocheModuleSpecifier = module_specifier.try_into()?;
        let (contents_tx, contents_rx) = std::sync::mpsc::channel();

        self.task
            .tx
            .send(ModuleLoaderMessage::ReadSpecifierContents {
                specifier: brioche_module_specifier.clone(),
                contents_tx,
            })?;
        let contents = contents_rx.recv()??;

        let code = std::str::from_utf8(&contents)
            .context("failed to parse module contents as UTF-8 string")?;

        let parsed = deno_ast::parse_module(deno_ast::ParseParams {
            specifier: brioche_module_specifier.clone().into(),
            text: code.into(),
            media_type: deno_ast::MediaType::TypeScript,
            capture_tokens: false,
            scope_analysis: false,
            maybe_syntax: None,
        })?;
        let transpiled = parsed.transpile(
            &deno_ast::TranspileOptions {
                imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Preserve,
                ..Default::default()
            },
            &deno_ast::EmitOptions {
                source_map: deno_ast::SourceMapOption::Separate,
                ..Default::default()
            },
        )?;

        if let Entry::Vacant(entry) = self
            .sources
            .borrow_mut()
            .entry(brioche_module_specifier.clone())
        {
            let source_map = transpiled
                .clone()
                .into_source()
                .source_map
                .context("source map not generated")?;
            entry.insert(ModuleSource {
                source_contents: contents.clone(),
                source_map,
            });
        }

        Ok(deno_core::ModuleSource::new(
            deno_core::ModuleType::JavaScript,
            deno_core::ModuleSourceCode::Bytes(
                transpiled.into_source().source.into_boxed_slice().into(),
            ),
            module_specifier,
            None,
        ))
    }
}

impl deno_core::ModuleLoader for BriocheModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: deno_core::ResolutionKind,
    ) -> Result<deno_core::ModuleSpecifier, anyhow::Error> {
        if let deno_core::ResolutionKind::MainModule = kind {
            let resolved = specifier.parse()?;
            tracing::debug!(%specifier, %referrer, %resolved, "resolved main module");
            return Ok(resolved);
        }

        let referrer: BriocheModuleSpecifier = referrer.parse()?;
        let specifier: BriocheImportSpecifier = specifier.parse()?;
        let (resolved_tx, resolved_rx) = std::sync::mpsc::channel();
        self.task.tx.send(ModuleLoaderMessage::ResolveSpecifier {
            specifier: specifier.clone(),
            referrer: referrer.clone(),
            resolved_tx,
        })?;
        let resolved = resolved_rx.recv()??;

        tracing::debug!(%specifier, %referrer, %resolved, "resolved module");

        let resolved: deno_core::ModuleSpecifier = resolved.into();
        Ok(resolved)
    }

    fn load(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
        _maybe_referrer: Option<&deno_core::ModuleSpecifier>,
        _is_dyn_import: bool,
        _requested_module_type: deno_core::RequestedModuleType,
    ) -> deno_core::ModuleLoadResponse {
        deno_core::ModuleLoadResponse::Sync(self.load_module_source(module_specifier))
    }

    fn get_source_map(&self, file_name: &str) -> Option<Vec<u8>> {
        let sources = self.sources.borrow();
        let specifier: BriocheModuleSpecifier = file_name.parse().ok()?;
        let code = sources.get(&specifier)?;
        Some(code.source_map.clone())
    }

    fn get_source_mapped_source_line(&self, file_name: &str, line_number: usize) -> Option<String> {
        let sources = self.sources.borrow();
        let specifier: BriocheModuleSpecifier = file_name.parse().ok()?;
        let source = sources.get(&specifier)?;
        let code = std::str::from_utf8(&source.source_contents)
            .inspect_err(|err| {
                tracing::warn!("failed to parse source contents of {file_name} as UTF-8: {err}");
            })
            .ok()?;
        let line = code.lines().nth(line_number)?;
        Some(line.to_string())
    }
}

struct ModuleSource {
    pub source_contents: Arc<Vec<u8>>,
    pub source_map: Vec<u8>,
}

pub enum ModuleLoaderMessage {
    ReadSpecifierContents {
        specifier: BriocheModuleSpecifier,
        contents_tx: std::sync::mpsc::Sender<anyhow::Result<Arc<Vec<u8>>>>,
    },
    ResolveSpecifier {
        specifier: BriocheImportSpecifier,
        referrer: BriocheModuleSpecifier,
        resolved_tx: std::sync::mpsc::Sender<anyhow::Result<BriocheModuleSpecifier>>,
    },
    ProjectRootForModulePath {
        module_path: PathBuf,
        project_path_tx: tokio::sync::oneshot::Sender<anyhow::Result<PathBuf>>,
    },
}

#[derive(Clone)]
pub struct RuntimeTask {
    tx: tokio::sync::mpsc::Sender<RuntimeMessage>,
}

impl RuntimeTask {
    pub fn new(brioche: Brioche, projects: Projects) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        tokio::task::spawn(async move {
            while let Some(message) = rx.recv().await {
                match message {
                    RuntimeMessage::BakeAll {
                        recipes,
                        bake_scope,
                        results_tx,
                    } => {
                        let results = futures::stream::iter(recipes)
                            .then(|recipe| async {
                                let brioche = brioche.clone();
                                super::bake::bake(&brioche, recipe, &bake_scope).await
                            })
                            .try_collect::<Vec<_>>()
                            .await;

                        let _ = results_tx.send(results);
                    }
                    RuntimeMessage::CreateProxy { recipe, result_tx } => {
                        let result = super::bake::create_proxy(&brioche, recipe).await;
                        let _ = result_tx.send(result);
                    }
                    RuntimeMessage::ReadBlob {
                        blob_hash,
                        result_tx,
                    } => {
                        let permit = crate::blob::get_save_blob_permit().await;
                        let permit = match permit {
                            Ok(permit) => permit,
                            Err(error) => {
                                let _ = result_tx.send(Err(error));
                                continue;
                            }
                        };

                        let path = crate::blob::blob_path(&brioche, permit, blob_hash).await;
                        let path = match path {
                            Ok(path) => path,
                            Err(error) => {
                                let _ = result_tx.send(Err(error));
                                continue;
                            }
                        };

                        let result = tokio::fs::read(path)
                            .await
                            .with_context(|| format!("failed to read blob {blob_hash}"));

                        let _ = result_tx.send(result);
                    }
                    RuntimeMessage::GetStatic {
                        specifier,
                        static_,
                        result_tx,
                    } => {
                        let recipe_hash = projects.get_static(&specifier, &static_);
                        let recipe_hash = match recipe_hash {
                            Ok(Some(recipe_hash)) => recipe_hash,
                            Ok(None) => {
                                let error = match static_ {
                                    StaticQuery::Include(StaticInclude::File { path }) => {
                                        anyhow::anyhow!("failed to resolve Brioche.includeFile({path:?}) from {specifier}, was the path passed in as a string literal?")
                                    }
                                    StaticQuery::Include(StaticInclude::Directory { path }) => {
                                        anyhow::anyhow!("failed to resolve Brioche.includeDirectory({path:?}) from {specifier}, was the path passed in as a string literal?")
                                    }
                                    StaticQuery::Glob { patterns } => {
                                        let patterns = patterns
                                            .iter()
                                            .map(|pattern| lazy_format::lazy_format!("{pattern:?}"))
                                            .join_with(", ");
                                        anyhow::anyhow!("failed to resolve Brioche.glob({patterns}) from {specifier}, were the patterns passed in as string literals?")
                                    }
                                    StaticQuery::Download { url } => {
                                        anyhow::anyhow!("failed to resolve Brioche.download({url:?}) from {specifier}, was the URL passed in as a string literal?")
                                    }
                                };
                                let _ = result_tx.send(Err(error));
                                continue;
                            }
                            Err(error) => {
                                let _ = result_tx.send(Err(error));
                                continue;
                            }
                        };

                        let result = crate::recipe::get_recipe(&brioche, recipe_hash).await;
                        let _ = result_tx.send(result);
                    }
                }
            }
        });

        Self { tx }
    }
}

pub enum RuntimeMessage {
    BakeAll {
        recipes: Vec<WithMeta<Recipe>>,
        bake_scope: BakeScope,
        results_tx: tokio::sync::oneshot::Sender<anyhow::Result<Vec<WithMeta<Artifact>>>>,
    },
    CreateProxy {
        recipe: Recipe,
        result_tx: tokio::sync::oneshot::Sender<anyhow::Result<Recipe>>,
    },
    ReadBlob {
        blob_hash: BlobHash,
        result_tx: tokio::sync::oneshot::Sender<anyhow::Result<Vec<u8>>>,
    },
    GetStatic {
        specifier: BriocheModuleSpecifier,
        static_: StaticQuery,
        result_tx: tokio::sync::oneshot::Sender<anyhow::Result<Recipe>>,
    },
}

deno_core::extension!(brioche_rt,
    ops = [
        op_brioche_bake_all,
        op_brioche_create_proxy,
        op_brioche_read_blob,
        op_brioche_get_static,
    ],
    options = {
        task: RuntimeTask,
        bake_scope: BakeScope,
    },
    state = |state, options| {
        state.put(options.task);
        state.put(options.bake_scope);
    },
);

#[deno_core::op2(async)]
#[serde]
pub async fn op_brioche_bake_all(
    state: Rc<RefCell<OpState>>,
    #[serde] recipes: Vec<WithMeta<Recipe>>,
) -> Result<Vec<Artifact>, deno_core::error::AnyError> {
    let runtime_task = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<RuntimeTask>()
            .context("failed to get runtime task")?
            .clone()
    };
    let bake_scope = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<BakeScope>()
            .context("failed to get bake scope")?
            .clone()
    };

    let (results_tx, results_rx) = tokio::sync::oneshot::channel();
    runtime_task
        .tx
        .send(RuntimeMessage::BakeAll {
            recipes: recipes.clone(),
            bake_scope: bake_scope.clone(),
            results_tx,
        })
        .await?;

    let results = results_rx.await??;
    let results = results.into_iter().map(|result| result.value).collect();

    // let mut results = vec![];
    // for recipe in recipes {
    //     let result = super::bake::bake(&brioche, recipe, &bake_scope).await?;
    //     results.push(result.value);
    // }
    Ok(results)
}

#[deno_core::op2(async)]
#[serde]
pub async fn op_brioche_create_proxy(
    state: Rc<RefCell<OpState>>,
    #[serde] recipe: Recipe,
) -> Result<Recipe, deno_core::error::AnyError> {
    let runtime_task = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<RuntimeTask>()
            .context("failed to get runtime task")?
            .clone()
    };

    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    runtime_task
        .tx
        .send(RuntimeMessage::CreateProxy { recipe, result_tx })
        .await?;

    let result = result_rx.await??;
    Ok(result)
}

// TODO: Return a Uint8Array instead of tick-encoding
#[deno_core::op2(async)]
#[serde]
pub async fn op_brioche_read_blob(
    state: Rc<RefCell<OpState>>,
    #[serde] blob_hash: BlobHash,
) -> Result<crate::encoding::TickEncode<Vec<u8>>, deno_core::error::AnyError> {
    let runtime_task = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<RuntimeTask>()
            .context("failed to get runtime task")?
            .clone()
    };

    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    runtime_task
        .tx
        .send(RuntimeMessage::ReadBlob {
            blob_hash,
            result_tx,
        })
        .await?;

    let bytes = result_rx.await??;

    // let permit = crate::blob::get_save_blob_permit().await?;
    // let path = crate::blob::blob_path(&brioche, permit, blob_hash).await?;
    // let bytes = tokio::fs::read(path)
    //     .await
    //     .with_context(|| format!("failed to read blob {blob_hash}"))?;

    Ok(crate::encoding::TickEncode(bytes))
}

#[deno_core::op2(async)]
#[serde]
pub async fn op_brioche_get_static(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[serde] static_: StaticQuery,
) -> Result<Recipe, deno_core::error::AnyError> {
    let runtime_task = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<RuntimeTask>()
            .context("failed to get runtime task")?
            .clone()
    };

    let specifier: BriocheModuleSpecifier = url.parse()?;

    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    runtime_task
        .tx
        .send(RuntimeMessage::GetStatic {
            specifier,
            static_,
            result_tx,
        })
        .await?;
    let recipe = result_rx.await??;

    // let recipe_hash = projects
    //     .get_static(&specifier, &static_)?
    //     .with_context(|| match static_ {
    //         StaticQuery::Include(StaticInclude::File { path }) => {
    //             format!("failed to resolve Brioche.includeFile({path:?}) from {specifier}, was the path passed in as a string literal?")
    //         }
    //         StaticQuery::Include(StaticInclude::Directory { path }) => {
    //             format!("failed to resolve Brioche.includeDirectory({path:?}) from {specifier}, was the path passed in as a string literal?")
    //         }
    //         StaticQuery::Glob { patterns } => {
    //             let patterns = patterns.iter().map(|pattern| lazy_format::lazy_format!("{pattern:?}")).join_with(", ");
    //             format!("failed to resolve Brioche.glob({patterns}) from {specifier}, were the patterns passed in as string literals?")
    //         }
    //         StaticQuery::Download { url } => {
    //             format!("failed to resolve Brioche.download({url:?}) from {specifier}, was the URL passed in as a string literal?")
    //         }

    //     })?;
    // let recipe = crate::recipe::get_recipe(&brioche, recipe_hash).await?;
    Ok(recipe)
}
