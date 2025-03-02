use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use joinery::JoinableIterator as _;
use tracing::Instrument as _;

use crate::{
    Brioche,
    bake::BakeScope,
    blob::BlobHash,
    project::{
        ProjectHash, ProjectLocking, ProjectValidation, Projects,
        analyze::{GitRefOptions, StaticInclude, StaticOutput, StaticOutputKind, StaticQuery},
    },
    recipe::{Artifact, DownloadRecipe, Recipe, WithMeta},
};

use super::specifier::{self, BriocheImportSpecifier, BriocheModuleSpecifier};

/// A type used to call Brioche functions across Tokio runtimes. This is used
/// to interact with Brioche from JavaScript code, due to restrictions that
/// require the V8 runtime to run in its own Tokio runtime.
///
/// This type works by spawning a Tokio task that uses a channel for
/// communicating across runtimes, which is explicitly supported by Tokio's
/// channel types.
#[derive(Clone)]
pub struct RuntimeBridge {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeBridgeMessage>,
}

impl RuntimeBridge {
    pub fn new(brioche: Brioche, projects: Projects) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let brioche = brioche.clone();
                let projects = projects.clone();

                tokio::spawn(async move {
                    match message {
                        RuntimeBridgeMessage::LoadProjectFromModulePath { path, result_tx } => {
                            let result = projects
                                .load_from_module_path(
                                    &brioche,
                                    &path,
                                    ProjectValidation::Minimal,
                                    ProjectLocking::Unlocked,
                                )
                                .await;
                            let _ = result_tx.send(result);
                        }
                        RuntimeBridgeMessage::LoadSpecifierContents {
                            specifier,
                            contents_tx,
                        } => {
                            let contents =
                                specifier::load_specifier_contents(&brioche.vfs, &specifier).await;
                            let _ = contents_tx.send(contents);
                        }
                        RuntimeBridgeMessage::ResolveSpecifier {
                            specifier,
                            referrer,
                            resolved_tx,
                        } => {
                            let resolved = specifier::resolve(&projects, &specifier, &referrer);
                            let _ = resolved_tx.send(resolved);
                        }
                        RuntimeBridgeMessage::UpdateVfsContents {
                            specifier,
                            contents,
                            result_tx,
                        } => {
                            let path = match specifier {
                                BriocheModuleSpecifier::Runtime { .. } => {
                                    let _ = result_tx.send(Ok(false));
                                    return;
                                }
                                BriocheModuleSpecifier::File { path } => path,
                            };

                            let file_id = match brioche.vfs.load_cached(&path) {
                                Ok(Some((file_id, _))) => file_id,
                                Ok(None) => {
                                    let _ = result_tx.send(Ok(false));
                                    return;
                                }
                                Err(error) => {
                                    let _ = result_tx.send(Err(error));
                                    return;
                                }
                            };

                            let result = brioche.vfs.update(file_id, contents).map(|_| true);
                            let _ = result_tx.send(result);
                        }
                        RuntimeBridgeMessage::ReloadProjectFromSpecifier {
                            specifier,
                            result_tx,
                        } => {
                            let path = match specifier {
                                BriocheModuleSpecifier::Runtime { .. } => {
                                    let _ = result_tx.send(Ok(false));
                                    return;
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
                                    return;
                                }
                            };

                            if let Some(project) = project {
                                let result = projects.clear(project).await;
                                match result {
                                    Ok(_) => {}
                                    Err(error) => {
                                        let _ = result_tx.send(Err(error));
                                        return;
                                    }
                                }
                            }

                            let result = projects
                                .load_from_module_path(
                                    &brioche,
                                    &path,
                                    ProjectValidation::Minimal,
                                    ProjectLocking::Unlocked,
                                )
                                .await
                                .map(|_| true);
                            let _ = result_tx.send(result);
                        }
                        RuntimeBridgeMessage::ReadSpecifierContents {
                            specifier,
                            contents_tx,
                        } => {
                            let contents =
                                specifier::read_specifier_contents(&brioche.vfs, &specifier);
                            let _ = contents_tx.send(contents);
                        }
                        RuntimeBridgeMessage::ProjectRootForModulePath {
                            module_path,
                            project_path_tx,
                        } => {
                            let project_hash = projects.find_containing_project(&module_path);
                            let project_hash = match project_hash {
                                Ok(Some(project_hash)) => project_hash,
                                Ok(None) => {
                                    let error = anyhow::anyhow!("containing project not found");
                                    let _ = project_path_tx.send(Err(error));
                                    return;
                                }
                                Err(error) => {
                                    let _ = project_path_tx
                                        .send(Err(error).context("failed to get project path"));
                                    return;
                                }
                            };
                            let project_path = projects.project_root(project_hash);
                            let project_path = match project_path {
                                Ok(project_path) => project_path,
                                Err(error) => {
                                    let _ = project_path_tx.send(Err(error));
                                    return;
                                }
                            };
                            let _ = project_path_tx.send(Ok(project_path));
                        }
                        RuntimeBridgeMessage::BakeAll {
                            recipes,
                            bake_scope,
                            results_tx,
                        } => {
                            let results = futures::stream::iter(recipes)
                                .then(|recipe| async {
                                    let brioche = brioche.clone();
                                    crate::bake::bake(&brioche, recipe, &bake_scope)
                                        .instrument(tracing::info_span!("runtime_bake_all"))
                                        .await
                                })
                                .try_collect::<Vec<_>>()
                                .await;

                            let _ = results_tx.send(results);
                        }
                        RuntimeBridgeMessage::CreateProxy { recipe, result_tx } => {
                            let result = crate::bake::create_proxy(&brioche, recipe).await;
                            let _ = result_tx.send(result);
                        }
                        RuntimeBridgeMessage::ReadBlob {
                            blob_hash,
                            result_tx,
                        } => {
                            let permit = crate::blob::get_save_blob_permit().await;
                            let mut permit = match permit {
                                Ok(permit) => permit,
                                Err(error) => {
                                    let _ = result_tx.send(Err(error));
                                    return;
                                }
                            };

                            let path =
                                crate::blob::blob_path(&brioche, &mut permit, blob_hash).await;
                            let path = match path {
                                Ok(path) => path,
                                Err(error) => {
                                    let _ = result_tx.send(Err(error));
                                    return;
                                }
                            };

                            drop(permit);

                            let result = tokio::fs::read(path)
                                .await
                                .with_context(|| format!("failed to read blob {blob_hash}"));

                            let _ = result_tx.send(result);
                        }
                        RuntimeBridgeMessage::GetStatic {
                            specifier,
                            static_,
                            result_tx,
                        } => {
                            let static_output = projects.get_static(&specifier, &static_);
                            let static_output = match static_output {
                                Ok(Some(static_output)) => static_output,
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
                                                .map(|pattern| {
                                                    lazy_format::lazy_format!("{pattern:?}")
                                                })
                                                .join_with(", ");
                                            anyhow::anyhow!("failed to resolve Brioche.glob({patterns}) from {specifier}, were the patterns passed in as string literals?")
                                        }
                                        StaticQuery::Download { url } => {
                                            anyhow::anyhow!("failed to resolve Brioche.download({url:?}) from {specifier}, was the URL passed in as a string literal?")
                                        }
                                        StaticQuery::GitRef(GitRefOptions { repository, ref_ }) => {
                                            anyhow::anyhow!("failed to resolve Brioche.gitRef({{ repository: \"{repository}\", ref: {ref_:?} }}) from {specifier}, were the repository and ref values passed in as string literals?")
                                        }
                                    };
                                    let _ = result_tx.send(Err(error));
                                    return;
                                }
                                Err(error) => {
                                    let _ = result_tx.send(Err(error));
                                    return;
                                }
                            };

                            let result = match static_output {
                                StaticOutput::RecipeHash(recipe_hash) => {
                                    let recipe =
                                        crate::recipe::get_recipe(&brioche, recipe_hash).await;
                                    let recipe = match recipe {
                                        Ok(recipe) => recipe,
                                        Err(error) => {
                                            let _ = result_tx.send(Err(error));
                                            return;
                                        }
                                    };

                                    GetStaticResult::Recipe(recipe)
                                }
                                StaticOutput::Kind(StaticOutputKind::Download { hash }) => {
                                    let StaticQuery::Download { url } = static_ else {
                                        let _ = result_tx.send(Err(anyhow::anyhow!("invalid 'download' static output kind for non-download static")));
                                        return;
                                    };

                                    GetStaticResult::Recipe(Recipe::Download(DownloadRecipe {
                                        url,
                                        hash,
                                    }))
                                }
                                StaticOutput::Kind(StaticOutputKind::GitRef { commit }) => {
                                    let StaticQuery::GitRef(git_ref) = static_ else {
                                        let _ = result_tx.send(Err(anyhow::anyhow!("invalid 'git_ref' static output kind for non-git ref static")));
                                        return;
                                    };

                                    GetStaticResult::GitRef {
                                        repository: git_ref.repository,
                                        commit,
                                    }
                                }
                            };
                            let _ = result_tx.send(Ok(result));
                        }
                    }
                }.instrument(tracing::Span::current()));
            }
        }.instrument(tracing::Span::current()));

        Self { tx }
    }

    pub async fn load_project_from_module_path(
        &self,
        path: std::path::PathBuf,
    ) -> anyhow::Result<ProjectHash> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        self.tx
            .send(RuntimeBridgeMessage::LoadProjectFromModulePath { path, result_tx })?;
        let result = result_rx.await??;
        Ok(result)
    }

    pub async fn load_specifier_contents(
        &self,
        specifier: BriocheModuleSpecifier,
    ) -> anyhow::Result<Arc<Vec<u8>>> {
        let (contents_tx, contents_rx) = tokio::sync::oneshot::channel();

        self.tx.send(RuntimeBridgeMessage::LoadSpecifierContents {
            specifier,
            contents_tx,
        })?;
        let contents = contents_rx.await??;
        Ok(contents)
    }

    pub fn resolve_specifier(
        &self,
        specifier: BriocheImportSpecifier,
        referrer: BriocheModuleSpecifier,
    ) -> anyhow::Result<BriocheModuleSpecifier> {
        let (resolved_tx, resolved_rx) = std::sync::mpsc::channel();

        self.tx.send(RuntimeBridgeMessage::ResolveSpecifier {
            specifier,
            referrer,
            resolved_tx,
        })?;
        let resolved = resolved_rx.recv()??;
        Ok(resolved)
    }

    pub async fn update_vfs_contents(
        &self,
        specifier: BriocheModuleSpecifier,
        contents: Arc<Vec<u8>>,
    ) -> anyhow::Result<bool> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        self.tx.send(RuntimeBridgeMessage::UpdateVfsContents {
            specifier,
            contents,
            result_tx,
        })?;
        let result = result_rx.await??;
        Ok(result)
    }

    pub async fn reload_project_from_specifier(
        &self,
        specifier: BriocheModuleSpecifier,
    ) -> anyhow::Result<bool> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        self.tx
            .send(RuntimeBridgeMessage::ReloadProjectFromSpecifier {
                specifier,
                result_tx,
            })?;
        let result = result_rx.await??;
        Ok(result)
    }

    pub fn read_specifier_contents(
        &self,
        specifier: BriocheModuleSpecifier,
    ) -> anyhow::Result<Arc<Vec<u8>>> {
        let (contents_tx, contents_rx) = std::sync::mpsc::channel();

        self.tx.send(RuntimeBridgeMessage::ReadSpecifierContents {
            specifier,
            contents_tx,
        })?;
        let contents = contents_rx.recv()??;
        Ok(contents)
    }

    pub async fn project_root_for_module_path(
        &self,
        module_path: PathBuf,
    ) -> anyhow::Result<PathBuf> {
        let (project_path_tx, project_path_rx) = tokio::sync::oneshot::channel();

        self.tx
            .send(RuntimeBridgeMessage::ProjectRootForModulePath {
                module_path,
                project_path_tx,
            })?;
        let project_path = project_path_rx.await??;
        Ok(project_path)
    }

    pub async fn bake_all(
        &self,
        recipes: Vec<WithMeta<Recipe>>,
        bake_scope: BakeScope,
    ) -> anyhow::Result<Vec<WithMeta<Artifact>>> {
        let (results_tx, results_rx) = tokio::sync::oneshot::channel();

        self.tx.send(RuntimeBridgeMessage::BakeAll {
            recipes,
            bake_scope,
            results_tx,
        })?;
        let results = results_rx.await??;
        Ok(results)
    }

    pub async fn create_proxy(&self, recipe: Recipe) -> anyhow::Result<Recipe> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        self.tx
            .send(RuntimeBridgeMessage::CreateProxy { recipe, result_tx })?;
        let result = result_rx.await??;
        Ok(result)
    }

    pub async fn read_blob(&self, blob_hash: BlobHash) -> anyhow::Result<Vec<u8>> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        self.tx.send(RuntimeBridgeMessage::ReadBlob {
            blob_hash,
            result_tx,
        })?;
        let result = result_rx.await??;
        Ok(result)
    }

    pub async fn get_static(
        &self,
        specifier: BriocheModuleSpecifier,
        static_: StaticQuery,
    ) -> anyhow::Result<GetStaticResult> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        self.tx.send(RuntimeBridgeMessage::GetStatic {
            specifier,
            static_,
            result_tx,
        })?;
        let result = result_rx.await??;
        Ok(result)
    }
}

enum RuntimeBridgeMessage {
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
    ReadSpecifierContents {
        specifier: BriocheModuleSpecifier,
        contents_tx: std::sync::mpsc::Sender<anyhow::Result<Arc<Vec<u8>>>>,
    },
    ProjectRootForModulePath {
        module_path: PathBuf,
        project_path_tx: tokio::sync::oneshot::Sender<anyhow::Result<PathBuf>>,
    },
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
        result_tx: tokio::sync::oneshot::Sender<anyhow::Result<GetStaticResult>>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "staticKind", rename_all = "snake_case")]
pub enum GetStaticResult {
    Recipe(Recipe),
    GitRef {
        repository: url::Url,
        commit: String,
    },
}
