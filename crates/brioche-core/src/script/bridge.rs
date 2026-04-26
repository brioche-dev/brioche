use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use joinery::JoinableIterator as _;
use tracing::Instrument as _;

use crate::{
    Brioche,
    bake::BakeScope,
    blob::BlobHash,
    project::{
        ProjectHash, ProjectLocking, ProjectValidation, Projects,
        analyze::{
            GitCheckoutQuery, GitRefOptions, StaticInclude, StaticOutput, StaticOutputKind,
            StaticQuery,
        },
    },
    recipe::{Artifact, DownloadRecipe, Recipe, WithMeta},
};

use super::specifier::{self, BriocheImportSpecifier, BriocheModuleSpecifier};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct ResolvedGitCheckoutOptions {
    keep_git_dir: bool,
    submodules: bool,
    tag: Option<String>,
}

fn resolve_git_checkout_options(
    query: &GitCheckoutQuery,
) -> anyhow::Result<ResolvedGitCheckoutOptions> {
    let tag = query.options.normalized_tag(&query.ref_);
    anyhow::ensure!(
        query.options.keep_git_dir || tag.is_none(),
        "Cannot use 'tag' without 'keepGitDir' enabled"
    );

    Ok(ResolvedGitCheckoutOptions {
        keep_git_dir: query.options.keep_git_dir,
        submodules: query.options.submodules,
        tag,
    })
}

fn git_checkout_output_dir(
    brioche: &Brioche,
    query: &GitCheckoutQuery,
    commit: &str,
) -> anyhow::Result<PathBuf> {
    let options = resolve_git_checkout_options(query)?;
    let mut cache_key_hasher = crate::Hasher::new_sha256();
    cache_key_hasher.update(query.repository.as_str().as_bytes());
    cache_key_hasher.update(b"\0");
    cache_key_hasher.update(commit.as_bytes());
    cache_key_hasher.update(b"\0");
    cache_key_hasher.update(&serde_json::to_vec(&options)?);
    let cache_key = match cache_key_hasher.finish()? {
        crate::Hash::Sha256 { value } => hex::encode(value),
    };

    Ok(brioche.data_dir.join("git-checkouts").join(cache_key))
}

async fn ensure_git_checkout(
    brioche: &Brioche,
    query: &GitCheckoutQuery,
    commit: &str,
) -> anyhow::Result<PathBuf> {
    let options = resolve_git_checkout_options(query)?;
    let output_dir = git_checkout_output_dir(brioche, query, commit)?;
    if !tokio::fs::try_exists(&output_dir).await? {
        crate::download::git_checkout(
            &query.repository,
            &query.ref_,
            commit,
            &output_dir,
            options.submodules,
            options.keep_git_dir,
            options.tag.as_deref(),
        )
        .await
        .with_context(|| {
            format!(
                "failed to checkout ref '{}' ({commit}) from git repo '{}'",
                query.ref_, query.repository
            )
        })?;
    }

    Ok(output_dir)
}

async fn import_directory_recipe(brioche: &Brioche, path: &Path) -> anyhow::Result<Recipe> {
    let mut saved_paths = HashMap::default();
    let meta = Arc::default();
    let artifact = crate::input::create_input(
        brioche,
        crate::input::InputOptions {
            input_path: path,
            remove_input: false,
            resource_dir: None,
            input_resource_dirs: &[],
            saved_paths: &mut saved_paths,
            meta: &meta,
        },
    )
    .await?;

    let crate::recipe::Artifact::Directory(_) = &artifact.value else {
        anyhow::bail!("git checkout at {} was not a directory", path.display());
    };

    let recipe = crate::recipe::Recipe::from(artifact.value);
    crate::recipe::save_recipes(brioche, [&recipe]).await?;

    Ok(recipe)
}

async fn load_git_checkout_recipe(
    brioche: &Brioche,
    query: &GitCheckoutQuery,
    commit: &str,
) -> anyhow::Result<Recipe> {
    let output_dir = ensure_git_checkout(brioche, query, commit).await?;
    import_directory_recipe(brioche, &output_dir).await
}

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

                            let result = brioche.vfs.update(file_id, contents).map(|()| true);
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
                                let result = projects.clear(project);
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
                            let bake_futures = recipes.into_iter().map(|recipe| {
                                let brioche = brioche.clone();
                                let bake_scope = bake_scope.clone();
                                async move {
                                    crate::bake::bake(&brioche, recipe, &bake_scope)
                                        .instrument(tracing::info_span!("runtime_bake_all"))
                                        .await
                                }
                            });

                            let results = futures::future::try_join_all(bake_futures).await;

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
                            let path =
                                crate::blob::blob_path(&brioche, blob_hash).await;
                            let path = match path {
                                Ok(path) => path,
                                Err(error) => {
                                    let _ = result_tx.send(Err(error));
                                    return;
                                }
                            };

                            let result = tokio::fs::read(path)
                                .await
                                .with_context(|| format!("failed to read blob {blob_hash}"));

                            let _ = result_tx.send(result);
                        }
                        RuntimeBridgeMessage::GetStatic {
                            specifier,
                            options,
                            result_tx,
                        } => {
                            let static_output = projects.get_static(&specifier, &options.query);
                            let callee = options.callee();
                            let static_output = match static_output {
                                Ok(Some(static_output)) => static_output,
                                Ok(None) => {
                                    let error = match &options.query {
                                        StaticQuery::Include(include) => {
                                            let path = include.path();
                                            anyhow::anyhow!("failed to resolve {callee}({path:?}) from {specifier}, was the path passed in as a string literal?")
                                        }
                                        StaticQuery::Glob { patterns } => {
                                            let patterns = patterns
                                                .iter()
                                                .map(|pattern| {
                                                    lazy_format::lazy_format!("{pattern:?}")
                                                })
                                                .join_with(", ");
                                            anyhow::anyhow!("failed to resolve {callee}({patterns}) from {specifier}, were the patterns passed in as string literals?")
                                        }
                                        StaticQuery::Download { url } => {
                                            anyhow::anyhow!("failed to resolve {callee}({url:?}) from {specifier}, was the URL passed in as a string literal?")
                                        }
                                        StaticQuery::GitRef(GitRefOptions { repository, ref_ }) => {
                                            anyhow::anyhow!("failed to resolve {callee}({{ repository: \"{repository}\", ref: {ref_:?} }}) from {specifier}, were the repository and ref values passed in as string literals?")
                                        }
                                        StaticQuery::GitCheckout(GitCheckoutQuery { repository, ref_, options: _ }) => {
                                            anyhow::anyhow!("failed to resolve {callee}({{ repository: \"{repository}\", ref: {ref_:?} }}) from {specifier}, were the repository and ref values passed in as string literals?")
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
                                    let StaticQuery::Download { url } = options.query else {
                                        let _ = result_tx.send(Err(anyhow::anyhow!("invalid 'download' static output kind for non-download static")));
                                        return;
                                    };

                                    GetStaticResult::Recipe(Recipe::Download(DownloadRecipe {
                                        url,
                                        hash,
                                    }))
                                }
                                StaticOutput::Kind(StaticOutputKind::GitRef { commit }) => {
                                    match options.query {
                                        StaticQuery::GitRef(git_ref) => GetStaticResult::GitRef {
                                            repository: git_ref.repository,
                                            commit,
                                        },
                                        StaticQuery::GitCheckout(git_checkout) => {
                                            let recipe = load_git_checkout_recipe(&brioche, &git_checkout, &commit)
                                            .await;
                                            let recipe = match recipe {
                                                Ok(recipe) => recipe,
                                                Err(error) => {
                                                    let _ = result_tx.send(Err(error));
                                                    return;
                                                }
                                            };

                                            GetStaticResult::Recipe(recipe)
                                        }
                                        _ => {
                                            let _ = result_tx.send(Err(anyhow::anyhow!("invalid 'git_ref' static output kind for non-git static")));
                                            return;
                                        }
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
        options: GetStaticOptions,
    ) -> anyhow::Result<GetStaticResult> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        self.tx.send(RuntimeBridgeMessage::GetStatic {
            specifier,
            options,
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
        options: GetStaticOptions,
        result_tx: tokio::sync::oneshot::Sender<anyhow::Result<GetStaticResult>>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStaticOptions {
    callee: Option<String>,
    #[serde(flatten)]
    query: StaticQuery,
}

impl GetStaticOptions {
    fn callee(&self) -> &str {
        if let Some(callee) = &self.callee {
            return callee;
        }

        match self.query {
            StaticQuery::Include(StaticInclude::File { .. }) => "Brioche.includeFile",
            StaticQuery::Include(StaticInclude::Directory { .. }) => "Brioche.includeDirectory",
            StaticQuery::Glob { .. } => "Brioche.glob",
            StaticQuery::Download { .. } => "Brioche.download",
            StaticQuery::GitRef(..) => "Brioche.gitRef",
            StaticQuery::GitCheckout(..) => "Brioche.gitCheckout",
        }
    }
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
