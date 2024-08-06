use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{Arc, RwLock},
};

use anyhow::Context as _;
use deno_core::OpState;

use crate::{
    project::{analyze::find_imports, ProjectHash, Projects},
    Brioche,
};

use super::specifier::{self, resolve, BriocheImportSpecifier, BriocheModuleSpecifier};

#[derive(Clone)]
pub struct CompilerHostTask {
    tx: tokio::sync::mpsc::UnboundedSender<CompilerHostMessage>,
}

impl CompilerHostTask {
    pub fn new(brioche: Brioche, projects: Projects) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        tokio::task::spawn(async move {
            while let Some(message) = rx.recv().await {
                match message {
                    CompilerHostMessage::LoadProjectFromModulePath { path, result_tx } => {
                        let result = projects.load_from_module_path(&brioche, &path, false).await;
                        let _ = result_tx.send(result);
                    }
                    CompilerHostMessage::LoadSpecifierContents {
                        specifier,
                        contents_tx,
                    } => {
                        let contents =
                            specifier::load_specifier_contents(&brioche.vfs, &specifier).await;
                        let _ = contents_tx.send(contents);
                    }
                    CompilerHostMessage::ResolveSpecifier {
                        specifier,
                        referrer,
                        resolved_tx,
                    } => {
                        let resolved = resolve(&projects, &specifier, &referrer);
                        let _ = resolved_tx.send(resolved);
                    }
                    CompilerHostMessage::UpdateVfsContents {
                        specifier,
                        contents,
                        result_tx,
                    } => {
                        let path = match specifier {
                            BriocheModuleSpecifier::Runtime { .. } => {
                                let _ = result_tx.send(Ok(false));
                                continue;
                            }
                            BriocheModuleSpecifier::File { path } => path,
                        };

                        let file_id = match brioche.vfs.load_cached(&path) {
                            Ok(Some((file_id, _))) => file_id,
                            Ok(None) => {
                                let _ = result_tx.send(Ok(false));
                                continue;
                            }
                            Err(error) => {
                                let _ = result_tx.send(Err(error));
                                continue;
                            }
                        };

                        let result = brioche.vfs.update(file_id, contents).map(|_| true);
                        let _ = result_tx.send(result);
                    }
                    CompilerHostMessage::ReloadProjectFromSpecifier {
                        specifier,
                        result_tx,
                    } => {
                        let path = match specifier {
                            BriocheModuleSpecifier::Runtime { .. } => {
                                let _ = result_tx.send(Ok(false));
                                continue;
                            }
                            BriocheModuleSpecifier::File { path } => path,
                        };

                        let project = projects.find_containing_project(&path);
                        let project = match project {
                            Ok(project) => project,
                            Err(error) => {
                                let _ = result_tx.send(Err(error).with_context(|| {
                                    format!("no project found for path '{}'", path.display())
                                }));
                                continue;
                            }
                        };

                        if let Some(project) = project {
                            let result = projects.clear(project).await;
                            match result {
                                Ok(_) => {}
                                Err(error) => {
                                    let _ = result_tx.send(Err(error));
                                    continue;
                                }
                            }
                        }

                        let result = projects
                            .load_from_module_path(&brioche, &path, false)
                            .await
                            .map(|_| true);
                        let _ = result_tx.send(result);
                    }
                }
            }
        });

        Self { tx }
    }
}

#[derive(Clone)]
pub struct BriocheCompilerHost {
    pub task: CompilerHostTask,
    pub documents: Arc<RwLock<HashMap<BriocheModuleSpecifier, BriocheDocument>>>,
}

impl BriocheCompilerHost {
    pub async fn new(task: CompilerHostTask) -> Self {
        let documents: HashMap<_, _> = specifier::runtime_specifiers_with_contents()
            .map(|(specifier, contents)| {
                let contents = std::str::from_utf8(&contents)
                    .map_err(|_| anyhow::anyhow!("invalid UTF-8 in runtime file: {specifier}"))
                    .unwrap();
                let document = BriocheDocument {
                    contents: Arc::new(contents.to_owned()),
                    version: 0,
                };
                (specifier, document)
            })
            .collect();

        Self {
            task,
            documents: Arc::new(RwLock::new(documents)),
        }
    }

    pub async fn load_documents(
        &self,
        specifiers: Vec<BriocheModuleSpecifier>,
    ) -> anyhow::Result<()> {
        let mut already_visited = HashSet::new();
        let mut specifiers_to_load = specifiers;

        while let Some(specifier) = specifiers_to_load.pop() {
            if !already_visited.insert(specifier.clone()) {
                continue;
            }

            let contents = {
                let documents = self
                    .documents
                    .read()
                    .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
                documents.get(&specifier).map(|doc| doc.contents.clone())
            };

            match &specifier {
                BriocheModuleSpecifier::File { path } => {
                    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                    self.task
                        .tx
                        .send(CompilerHostMessage::LoadProjectFromModulePath {
                            path: path.clone(),
                            result_tx,
                        })?;
                    result_rx.await??;
                }
                BriocheModuleSpecifier::Runtime { .. } => {}
            }

            let contents = match contents {
                Some(contents) => contents,
                None => {
                    let (contents_tx, contents_rx) = tokio::sync::oneshot::channel();
                    self.task
                        .tx
                        .send(CompilerHostMessage::LoadSpecifierContents {
                            specifier: specifier.clone(),
                            contents_tx,
                        })?;
                    let contents = contents_rx.await??;
                    // let contents =
                    //     super::specifier::load_specifier_contents(&self.brioche.vfs, &specifier)
                    //         .await?;
                    let contents = std::str::from_utf8(&contents).with_context(|| {
                        format!("failed to parse module '{specifier}' contents as UTF-8 string")
                    })?;

                    let mut documents = self
                        .documents
                        .write()
                        .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;

                    match documents.entry(specifier.clone()) {
                        std::collections::hash_map::Entry::Occupied(entry) => {
                            entry.get().contents.clone()
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            tracing::debug!("loaded new document into compiler host: {specifier}");
                            let contents = Arc::new(contents.to_string());
                            entry.insert(BriocheDocument {
                                contents: contents.clone(),
                                version: 0,
                            });
                            contents
                        }
                    }
                }
            };

            let import_specifiers = {
                let parsed = biome_js_parser::parse(
                    &contents,
                    biome_js_syntax::JsFileSource::ts()
                        .with_module_kind(biome_js_syntax::ModuleKind::Module),
                    biome_js_parser::JsParserOptions::default(),
                )
                .cast::<biome_js_syntax::JsModule>()
                .expect("failed to cast module");

                let Some(parsed_module) = parsed.try_tree() else {
                    tracing::warn!("failed to parse module {specifier}");
                    return Ok(());
                };

                find_imports(&parsed_module, |_| "<unknown>").collect::<Vec<_>>()
            };

            for import_specifier in import_specifiers {
                let import_specifier = match import_specifier {
                    Ok(import_specifier) => import_specifier,
                    Err(error) => {
                        tracing::warn!("error parsing import specifier: {error:#}");
                        continue;
                    }
                };
                // let resolved = resolve(&self.projects, &import_specifier, &specifier);
                let (resolved_tx, resolved_rx) = std::sync::mpsc::channel();
                self.task.tx.send(CompilerHostMessage::ResolveSpecifier {
                    specifier: import_specifier.clone(),
                    referrer: specifier.clone(),
                    resolved_tx,
                })?;
                let resolved = resolved_rx.recv()?;
                let resolved = match resolved {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        tracing::warn!("error resolving import specifier: {error:#}");
                        continue;
                    }
                };

                specifiers_to_load.push(resolved.clone());
            }
        }

        Ok(())
    }

    pub async fn update_document(&self, uri: &url::Url, contents: &str) -> anyhow::Result<()> {
        let specifier: BriocheModuleSpecifier = uri.try_into()?;

        {
            let mut documents = self
                .documents
                .write()
                .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
            documents
                .entry(specifier.clone())
                .and_modify(|doc| {
                    if *doc.contents != contents {
                        doc.version += 1;

                        let doc_contents = Arc::make_mut(&mut doc.contents);
                        contents.clone_into(doc_contents);
                    }
                })
                .or_insert_with(|| {
                    tracing::debug!("inserted new document into compiler host: {specifier}");
                    BriocheDocument {
                        contents: Arc::new(contents.to_owned()),
                        version: 0,
                    }
                });
        }

        // match &specifier {
        //     BriocheModuleSpecifier::Runtime { .. } => {}
        //     BriocheModuleSpecifier::File { path } => {
        //         if let Some((file_id, _)) = self.brioche.vfs.load_cached(path)? {
        //             self.brioche
        //                 .vfs
        //                 .update(file_id, Arc::new(contents.as_bytes().to_vec()))?;
        //         };
        //     }
        // }
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.task.tx.send(CompilerHostMessage::UpdateVfsContents {
            specifier: specifier.clone(),
            contents: Arc::new(contents.as_bytes().to_vec()),
            result_tx,
        })?;
        result_rx.await??;

        self.load_documents(vec![specifier]).await?;

        Ok(())
    }

    pub fn read_loaded_document<R>(
        &self,
        specifier: BriocheModuleSpecifier,
        f: impl FnOnce(&BriocheDocument) -> R,
    ) -> anyhow::Result<Option<R>> {
        let documents = self
            .documents
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
        let Some(document) = documents.get(&specifier) else {
            return Ok(None);
        };

        let result = f(document);
        Ok(Some(result))
    }

    pub async fn reload_module_project(&self, uri: &url::Url) -> anyhow::Result<()> {
        let specifier: BriocheModuleSpecifier = uri.try_into()?;

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.task
            .tx
            .send(CompilerHostMessage::ReloadProjectFromSpecifier {
                specifier: specifier.clone(),
                result_tx,
            })?;

        let did_update = result_rx.await??;

        if did_update {
            let mut documents = self
                .documents
                .write()
                .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
            documents.entry(specifier.clone()).and_modify(|doc| {
                doc.version += 1;
            });
        }

        //     let mut documents = self
        //     .documents
        //     .write()
        //     .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
        // documents.entry(specifier.clone()).and_modify(|doc| {
        //     doc.version += 1;
        // });

        // match &specifier {
        //     BriocheModuleSpecifier::File { path } => {
        //         let project = self
        //             .projects
        //             .find_containing_project(path)
        //             .with_context(|| format!("no project found for path '{}'", path.display()))?;

        //         if let Some(project) = project {
        //             self.projects.clear(project).await?;
        //         }

        //         self.projects
        //             .load_from_module_path(&self.brioche, path, false)
        //             .await?;

        //         let mut documents = self
        //             .documents
        //             .write()
        //             .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
        //         documents.entry(specifier.clone()).and_modify(|doc| {
        //             doc.version += 1;
        //         });
        //     }
        //     BriocheModuleSpecifier::Runtime { .. } => {}
        // }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BriocheDocument {
    pub contents: Arc<String>,
    pub version: u64,
}

pub enum CompilerHostMessage {
    LoadProjectFromModulePath {
        path: std::path::PathBuf,
        result_tx: tokio::sync::oneshot::Sender<anyhow::Result<ProjectHash>>,
    },
    LoadSpecifierContents {
        specifier: BriocheModuleSpecifier,
        contents_tx: tokio::sync::oneshot::Sender<anyhow::Result<Arc<Vec<u8>>>>,
    },
    ResolveSpecifier {
        specifier: BriocheImportSpecifier,
        referrer: BriocheModuleSpecifier,
        resolved_tx: std::sync::mpsc::Sender<anyhow::Result<BriocheModuleSpecifier>>,
    },
    UpdateVfsContents {
        specifier: BriocheModuleSpecifier,
        contents: Arc<Vec<u8>>,
        result_tx: tokio::sync::oneshot::Sender<anyhow::Result<bool>>,
    },
    ReloadProjectFromSpecifier {
        specifier: BriocheModuleSpecifier,
        result_tx: tokio::sync::oneshot::Sender<anyhow::Result<bool>>,
    },
}

deno_core::extension!(brioche_compiler_host,
    ops = [
        op_brioche_file_read,
        op_brioche_file_exists,
        op_brioche_file_version,
        op_brioche_resolve_module,
    ],
    options = {
        compiler_host: BriocheCompilerHost,
    },
    state = |state, options| {
        state.put(options.compiler_host);
    },
);

fn brioche_compiler_host_state(state: Rc<RefCell<OpState>>) -> anyhow::Result<BriocheCompilerHost> {
    let state = state.try_borrow()?;
    let compiler_host = state
        .try_borrow::<BriocheCompilerHost>()
        .context("failed to get compiler host instance")?
        .clone();

    Ok(compiler_host)
}

#[deno_core::op2]
#[serde]
pub fn op_brioche_file_read(
    state: Rc<RefCell<OpState>>,
    #[string] path: &str,
) -> Result<Option<Arc<String>>, deno_core::error::AnyError> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let contents = compiler_host.read_loaded_document(specifier, |doc| doc.contents.clone())?;
    Ok(contents)
}

#[deno_core::op2(fast)]
pub fn op_brioche_file_exists(
    state: Rc<RefCell<OpState>>,
    #[string] path: &str,
) -> Result<bool, deno_core::error::AnyError> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let result = compiler_host.read_loaded_document(specifier, |_| ())?;
    Ok(result.is_some())
}

#[deno_core::op2]
#[bigint]
pub fn op_brioche_file_version(
    state: Rc<RefCell<OpState>>,
    #[string] path: &str,
) -> Result<Option<u64>, deno_core::error::AnyError> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let version = compiler_host.read_loaded_document(specifier, |doc| doc.version)?;
    Ok(version)
}

#[deno_core::op2]
#[string]
pub fn op_brioche_resolve_module(
    state: Rc<RefCell<OpState>>,
    #[string] specifier: &str,
    #[string] referrer: &str,
) -> Option<String> {
    let compiler_host = brioche_compiler_host_state(state).ok()?;

    let referrer: BriocheModuleSpecifier = referrer.parse().ok()?;
    let specifier: BriocheImportSpecifier = specifier.parse().ok()?;
    let (resolved_tx, resolved_rx) = std::sync::mpsc::channel();

    let _ = compiler_host
        .task
        .tx
        .send(CompilerHostMessage::ResolveSpecifier {
            specifier,
            referrer,
            resolved_tx,
        });
    let resolved = resolved_rx.recv().ok()?.ok()?;
    // let resolved =
    //     super::specifier::resolve(&compiler_host.projects, &specifier, &referrer).ok()?;

    Some(resolved.to_string())
}
