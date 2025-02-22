#![allow(unused)]

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Output,
    sync::{atomic::AtomicU32, Arc, OnceLock},
};

use anyhow::Context as _;
use brioche_core::{
    blob::{BlobHash, SaveBlobOptions},
    project::{self, ProjectHash, ProjectLocking, ProjectValidation, Projects},
    recipe::{
        CreateDirectory, Directory, DownloadRecipe, File, ProcessRecipe, ProcessTemplate,
        ProcessTemplateComponent, Recipe, WithMeta,
    },
    Brioche, BriocheBuilder,
};
use futures::{FutureExt as _, StreamExt as _, TryFutureExt as _, TryStreamExt as _};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _},
    sync::Mutex,
};
use tower_lsp::lsp_types::{self, notification, request};

pub async fn brioche_test() -> (Brioche, TestContext) {
    brioche_test_with(|builder| builder).await
}

pub async fn brioche_test_with(
    f: impl FnOnce(BriocheBuilder) -> BriocheBuilder,
) -> (Brioche, TestContext) {
    let temp = tempdir::TempDir::new("brioche-test").unwrap();
    let registry_server = mockito::Server::new_async().await;

    let brioche_data_dir = temp.path().join("brioche-data");
    tokio::fs::create_dir_all(&brioche_data_dir)
        .await
        .expect("failed to create brioche data dir");
    let brioche_data_dir = tokio::fs::canonicalize(&brioche_data_dir)
        .await
        .expect("failed to canonicalize brioche data dir path");

    let (reporter, reporter_guard) = brioche_core::reporter::start_test_reporter();
    let builder = BriocheBuilder::new(reporter)
        .config(brioche_core::config::BriocheConfig::default())
        .data_dir(brioche_data_dir)
        .registry_client(brioche_core::registry::RegistryClient::new_with_client(
            reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build(),
            registry_server.url().parse().unwrap(),
            brioche_core::registry::RegistryAuthentication::Admin {
                password: "admin".to_string(),
            },
        ))
        .cache_client(brioche_core::cache::CacheClient::default())
        .self_exec_processes(false);
    let builder = f(builder);
    let brioche = builder.build().await.unwrap();
    let context = TestContext {
        brioche: brioche.clone(),
        temp,
        registry_server,
        _reporter_guard: reporter_guard,
    };
    (brioche, context)
}

/// Pre-load rootfs used when baking processes into `brioche`. Each file
/// will be downloaded to a temporary path and re-used within the same
/// test run.
pub async fn load_rootfs_recipes(brioche: &Brioche, platform: brioche_core::platform::Platform) {
    let brioche_core::bake::ProcessRootfsRecipes {
        sh,
        env,
        utils,
        proot,
    } = brioche_core::bake::process_rootfs_recipes(platform);

    let mut recipes = vec![sh, env, utils];
    if let Some(proot) = proot {
        recipes.push(proot);
    }

    brioche_core::recipe::save_recipes(brioche, recipes.clone())
        .await
        .unwrap();

    // Get all inner download recipes from the returned rootfs recipes
    let mut recipes = VecDeque::from_iter(recipes);
    let mut download_recipes = vec![];
    while let Some(recipe) = recipes.pop_front() {
        match recipe {
            Recipe::Download(download) => {
                download_recipes.push(download);
            }
            Recipe::Unarchive(unarchive) => {
                recipes.push_back(unarchive.file.value);
            }
            Recipe::Process(_) => unimplemented!(),
            Recipe::CompleteProcess(_) => unimplemented!(),
            Recipe::CreateFile {
                content: _,
                executable: _,
                resources,
            } => {
                recipes.push_back(resources.value);
            }
            Recipe::CreateDirectory(directory) => {
                recipes.extend(directory.entries.into_values().map(|recipe| recipe.value));
            }
            Recipe::Cast { recipe, to: _ } => {
                recipes.push_back(recipe.value);
            }
            Recipe::Merge { directories } => {
                recipes.extend(directories.into_iter().map(|recipe| recipe.value))
            }
            Recipe::Peel {
                directory,
                depth: _,
            } => recipes.push_back(directory.value),
            Recipe::Get { directory, path: _ } => {
                recipes.push_back(directory.value);
            }
            Recipe::Insert {
                directory,
                path: _,
                recipe,
            } => {
                recipes.push_back(directory.value);
                if let Some(recipe) = recipe {
                    recipes.push_back(recipe.value);
                }
            }
            Recipe::Glob {
                directory,
                patterns: _,
            } => {
                recipes.push_back(directory.value);
            }
            Recipe::SetPermissions {
                file,
                executable: _,
            } => {
                recipes.push_back(file.value);
            }
            Recipe::CollectReferences { recipe } => {
                recipes.push_back(recipe.value);
            }
            Recipe::AttachResources { recipe } => {
                recipes.push_back(recipe.value);
            }
            Recipe::Proxy(_) => unimplemented!(),
            Recipe::Sync { recipe } => {
                recipes.push_back(recipe.value);
            }
            Recipe::File {
                content_blob: _,
                executable: _,
                resources: _,
            } => unimplemented!(),
            Recipe::Directory(_) => unimplemented!(),
            Recipe::Symlink { target: _ } => {}
        }
    }

    for download in download_recipes {
        cached_download(brioche, &download).await;
    }
}

async fn cached_download(
    brioche: &Brioche,
    download: &brioche_core::recipe::DownloadRecipe,
) -> tokio::fs::File {
    let dirs = directories::ProjectDirs::from("dev", "brioche", "brioche-test-support")
        .expect("failed to get path to cache download dir");
    let cached_downloads_dir = dirs.cache_dir().join("downloads");

    // Get the path to the cached file
    let filename_safe_hash = match &download.hash {
        brioche_core::Hash::Sha256 { value } => format!("sha256-{}", hex::encode(value)),
    };
    let mut cached_path = cached_downloads_dir.join(format!("download-{filename_safe_hash}"));

    let cached_file = tokio::fs::File::open(&cached_path).await;
    match cached_file {
        Ok(mut file) => {
            // Cached file already exists, so we need to save it as a blob,
            // then record that the recipe bakes to the same file

            // Save the file as a blob
            let mut permit = brioche_core::blob::get_save_blob_permit().await.unwrap();
            let blob_hash = brioche_core::blob::save_blob_from_reader(
                brioche,
                &mut permit,
                &mut file,
                brioche_core::blob::SaveBlobOptions::default()
                    .expected_hash(Some(download.hash.clone())),
                &mut vec![],
            )
            .await
            .unwrap();

            // Save a bake result, so the download maps to a file with
            // the saved blob
            let input_recipe = brioche_core::recipe::Recipe::Download(download.clone());
            let output_recipe = brioche_core::recipe::Recipe::File {
                content_blob: blob_hash,
                executable: false,
                resources: Box::new(WithMeta::without_meta(
                    brioche_core::recipe::Recipe::Directory(Default::default()),
                )),
            };
            let input_hash = input_recipe.hash();
            let input_json = serde_json::to_string(&input_recipe).unwrap();
            let output_hash = output_recipe.hash();
            let output_json = serde_json::to_string(&output_recipe).unwrap();
            brioche_core::bake::save_bake_result(
                brioche,
                input_hash,
                &input_json,
                output_hash,
                &output_json,
            )
            .await
            .unwrap();

            file.rewind().await.expect("failed to rewind file");
            file
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Download isn't cached, so download it

            // Download the recipe
            let blob_hash = brioche_core::download::download(
                brioche,
                &download.url,
                Some(download.hash.clone()),
            )
            .await;
            let blob_hash = match blob_hash {
                Ok(blob_hash) => blob_hash,
                Err(error) => {
                    panic!("failed to download blob from {}: {error:#}", download.url);
                }
            };
            let blob_path = brioche_core::blob::local_blob_path(brioche, blob_hash);

            // Save the downloaded blob to the cache
            tokio::fs::create_dir_all(&cached_downloads_dir)
                .await
                .expect("failed to create cache download dir");
            tokio::fs::copy(&blob_path, &cached_path)
                .await
                .context("failed to save cached file");

            // Return the cached file
            tokio::fs::File::open(&cached_path)
                .await
                .expect("failed to open cached file")
        }
        Err(error) => {
            panic!(
                "failed to open cached file {}: {error:#}",
                cached_path.display()
            );
        }
    }
}

pub async fn load_project(
    brioche: &Brioche,
    path: &Path,
) -> anyhow::Result<(Projects, ProjectHash)> {
    let projects = Projects::default();
    let project_hash = projects
        .load(
            brioche,
            path,
            ProjectValidation::Standard,
            ProjectLocking::Unlocked,
        )
        .await?;

    Ok((projects, project_hash))
}

pub async fn load_project_no_validate(
    brioche: &Brioche,
    path: &Path,
) -> anyhow::Result<(Projects, ProjectHash)> {
    let projects = Projects::default();
    let project_hash = projects
        .load(
            brioche,
            path,
            ProjectValidation::Minimal,
            ProjectLocking::Unlocked,
        )
        .await?;

    Ok((projects, project_hash))
}

pub async fn bake_without_meta(
    brioche: &Brioche,
    recipe: brioche_core::recipe::Recipe,
) -> anyhow::Result<brioche_core::recipe::Artifact> {
    let artifact = brioche_core::bake::bake(
        brioche,
        without_meta(recipe),
        &brioche_core::bake::BakeScope::Anonymous,
    )
    .await?;
    Ok(artifact.value)
}

pub async fn mock_bake(
    brioche: &Brioche,
    input: &brioche_core::recipe::Recipe,
    output: &brioche_core::recipe::Artifact,
) {
    let input_json = serde_json::to_string(input).expect("failed to serialize input recipe");
    let output_json = serde_json::to_string(output).expect("failed to serialize output artifact");
    let did_insert = brioche_core::bake::save_bake_result(
        brioche,
        input.hash(),
        &input_json,
        output.hash(),
        &output_json,
    )
    .await
    .expect("failed to save bake result");

    assert!(did_insert, "failed to stub bake result: already exists");
}

pub async fn blob(brioche: &Brioche, content: impl AsRef<[u8]> + std::marker::Unpin) -> BlobHash {
    let mut permit = brioche_core::blob::get_save_blob_permit().await.unwrap();
    brioche_core::blob::save_blob_from_reader(
        brioche,
        &mut permit,
        content.as_ref(),
        SaveBlobOptions::default(),
        &mut Vec::new(),
    )
    .await
    .unwrap()
}

pub fn lazy_file(blob: BlobHash, executable: bool) -> brioche_core::recipe::Recipe {
    brioche_core::recipe::Recipe::File {
        content_blob: blob,
        executable,
        resources: Box::new(WithMeta::without_meta(
            brioche_core::recipe::Recipe::Directory(Directory::default()),
        )),
    }
}

pub fn lazy_file_with_resources(
    blob: BlobHash,
    executable: bool,
    resources: brioche_core::recipe::Recipe,
) -> brioche_core::recipe::Recipe {
    brioche_core::recipe::Recipe::File {
        content_blob: blob,
        executable,
        resources: Box::new(WithMeta::without_meta(resources)),
    }
}

pub fn file(blob: BlobHash, executable: bool) -> brioche_core::recipe::Artifact {
    brioche_core::recipe::Artifact::File(File {
        content_blob: blob,
        executable,
        resources: Directory::default(),
    })
}

pub fn file_with_resources(
    blob: BlobHash,
    executable: bool,
    resources: brioche_core::recipe::Directory,
) -> brioche_core::recipe::Artifact {
    brioche_core::recipe::Artifact::File(File {
        content_blob: blob,
        executable,
        resources,
    })
}

pub fn lazy_dir_value<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche_core::recipe::Recipe)>,
) -> brioche_core::recipe::CreateDirectory {
    CreateDirectory {
        entries: entries
            .into_iter()
            .map(|(k, v)| (k.as_ref().into(), without_meta(v)))
            .collect(),
    }
}

pub fn lazy_dir<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche_core::recipe::Recipe)>,
) -> brioche_core::recipe::Recipe {
    brioche_core::recipe::Recipe::CreateDirectory(CreateDirectory {
        entries: entries
            .into_iter()
            .map(|(k, v)| (k.as_ref().into(), WithMeta::without_meta(v)))
            .collect(),
    })
}

pub fn empty_dir_value() -> brioche_core::recipe::Directory {
    brioche_core::recipe::Directory::default()
}

pub async fn dir_value<K: AsRef<[u8]>>(
    brioche: &Brioche,
    entries: impl IntoIterator<Item = (K, brioche_core::recipe::Artifact)>,
) -> brioche_core::recipe::Directory {
    let mut directory = Directory::default();
    for (k, v) in entries {
        directory
            .insert(brioche, k.as_ref(), Some(v))
            .await
            .expect("failed to insert into dir");
    }

    directory
}

pub async fn dir<K: AsRef<[u8]>>(
    brioche: &Brioche,
    entries: impl IntoIterator<Item = (K, brioche_core::recipe::Artifact)>,
) -> brioche_core::recipe::Artifact {
    brioche_core::recipe::Artifact::Directory(dir_value(brioche, entries).await)
}

pub fn lazy_dir_empty() -> brioche_core::recipe::Recipe {
    brioche_core::recipe::Recipe::CreateDirectory(CreateDirectory::default())
}

pub fn dir_empty() -> brioche_core::recipe::Artifact {
    brioche_core::recipe::Artifact::Directory(Directory::default())
}

pub fn lazy_symlink(target: impl AsRef<[u8]>) -> brioche_core::recipe::Recipe {
    brioche_core::recipe::Recipe::Symlink {
        target: target.as_ref().into(),
    }
}

pub fn symlink(target: impl AsRef<[u8]>) -> brioche_core::recipe::Artifact {
    brioche_core::recipe::Artifact::Symlink {
        target: target.as_ref().into(),
    }
}

pub fn without_meta<T>(value: T) -> WithMeta<T> {
    WithMeta::without_meta(value)
}

pub fn sha256(value: impl AsRef<[u8]>) -> brioche_core::Hash {
    let mut hasher = brioche_core::Hasher::Sha256(Default::default());
    hasher.update(value.as_ref());
    hasher.finish().unwrap()
}

pub fn default_process_x86_64_linux() -> ProcessRecipe {
    ProcessRecipe {
        command: ProcessTemplate { components: vec![] },
        args: vec![],
        env: BTreeMap::new(),
        dependencies: vec![],
        work_dir: Box::new(WithMeta::without_meta(Recipe::Directory(
            Directory::default(),
        ))),
        output_scaffold: None,
        platform: brioche_core::platform::Platform::X86_64Linux,
        is_unsafe: false,
        networking: false,
    }
}

pub fn default_process() -> ProcessRecipe {
    ProcessRecipe {
        command: ProcessTemplate { components: vec![] },
        args: vec![],
        env: BTreeMap::new(),
        dependencies: vec![],
        work_dir: Box::new(WithMeta::without_meta(Recipe::Directory(
            Directory::default(),
        ))),
        output_scaffold: None,
        platform: brioche_core::platform::current_platform(),
        is_unsafe: false,
        networking: false,
    }
}

pub fn tpl(s: impl AsRef<[u8]>) -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::Literal {
            value: s.as_ref().into(),
        }],
    }
}

pub fn output_path() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::OutputPath],
    }
}

pub fn home_dir() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::HomeDir],
    }
}

pub fn resource_dir() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::ResourceDir],
    }
}

pub fn input_resource_dirs() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::InputResourceDirs],
    }
}

pub fn work_dir() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::WorkDir],
    }
}

pub fn temp_dir() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::TempDir],
    }
}

pub fn template_input(input: brioche_core::recipe::Recipe) -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::Input {
            recipe: without_meta(input),
        }],
    }
}

pub fn tpl_join(templates: impl IntoIterator<Item = ProcessTemplate>) -> ProcessTemplate {
    ProcessTemplate {
        components: templates
            .into_iter()
            .flat_map(|template| template.components)
            .collect(),
    }
}

pub fn new_cache() -> Arc<dyn object_store::ObjectStore> {
    Arc::new(object_store::memory::InMemory::new())
}

pub struct TestContext {
    brioche: Brioche,
    temp: tempdir::TempDir,
    pub registry_server: mockito::ServerGuard,
    _reporter_guard: brioche_core::reporter::ReporterGuard,
}

impl TestContext {
    pub fn path(&self, path: impl AsRef<Path>) -> PathBuf {
        let temp_path = self
            .temp
            .path()
            .canonicalize()
            .expect("failed to canonicalize temp path");
        temp_path.join(path)
    }

    pub async fn mkdir(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = self.path(path.as_ref());
        tokio::fs::create_dir_all(&path).await.unwrap();
        path
    }

    pub async fn write_file(&self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(path.as_ref());

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }

        tokio::fs::write(&path, contents.as_ref()).await.unwrap();

        path
    }

    pub async fn write_symlink(&self, src: impl AsRef<Path>, dst: impl AsRef<Path>) -> PathBuf {
        let dst = self.path(dst.as_ref());

        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }

        tokio::fs::symlink(&src, &dst).await.unwrap();

        dst
    }

    pub async fn write_lockfile(
        &self,
        path: impl AsRef<Path>,
        contents: &brioche_core::project::Lockfile,
    ) -> PathBuf {
        self.write_file(path, serde_json::to_string_pretty(&contents).unwrap())
            .await
    }

    pub async fn write_toml<T>(&self, path: impl AsRef<Path>, contents: &T) -> PathBuf
    where
        T: serde::Serialize,
    {
        self.write_file(path, toml::to_string_pretty(&contents).unwrap())
            .await
    }

    pub async fn temp_project<F, Fut>(&self, f: F) -> (Projects, ProjectHash, PathBuf)
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let temp_project_path = self
            .mkdir(format!("temp-project-{}", ulid::Ulid::new()))
            .await;

        f(temp_project_path.clone()).await;

        let projects = Projects::default();
        let project_hash = projects
            .load(
                &self.brioche,
                &temp_project_path,
                ProjectValidation::Standard,
                ProjectLocking::Unlocked,
            )
            .await
            .expect("failed to load temp project");
        projects.commit_dirty_lockfiles().await.unwrap();

        (projects, project_hash, temp_project_path)
    }

    pub async fn local_registry_project<F, Fut>(&self, f: F) -> (ProjectHash, PathBuf)
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let (_, project_hash, temp_project_path) = self.temp_project(f).await;

        let project_path = self
            .mkdir(format!("brioche-data/projects/{project_hash}"))
            .await;
        tokio::fs::rename(&temp_project_path, &project_path)
            .await
            .expect("failed to rename temp project to final location");

        (project_hash, project_path)
    }

    pub async fn cached_registry_project<F, Fut>(
        &mut self,
        cache: &Arc<dyn object_store::ObjectStore>,
        f: F,
    ) -> ProjectHash
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        // Create a temporary test context so the project does not get
        // loaded into the current context. We still use the current context
        // to create the mocks
        let (brioche, context) = brioche_test_with({
            let cache = cache.clone();
            |builder| {
                builder
                    .registry_client(self.brioche.registry_client.clone())
                    .cache_client(brioche_core::cache::CacheClient {
                        store: Some(cache),
                        writable: true,
                        ..Default::default()
                    })
            }
        })
        .await;

        let (projects, project_hash, _) = context.temp_project(f).await;

        let project_artifact = brioche_core::project::artifact::create_artifact_with_projects(
            &brioche,
            &projects,
            &[project_hash],
        )
        .await
        .expect("failed to create artifact for project");
        let project_artifact = brioche_core::recipe::Artifact::Directory(project_artifact);
        let project_artifact_hash = project_artifact.hash();
        brioche_core::cache::save_artifact(&brioche, project_artifact)
            .await
            .expect("failed to save artifact to cache");
        brioche_core::cache::save_project_artifact_hash(
            &brioche,
            project_hash,
            project_artifact_hash,
        )
        .await
        .expect("failed to save project artifact hash to cache");

        project_hash
    }

    pub async fn remote_registry_project_<F, Fut>(&mut self, f: F) -> ProjectHash
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        // Create a temporary test context so the project does not get
        // loaded into the current context. We still use the current context
        // to create the mocks
        let (brioche, context) = brioche_test_with(|builder| {
            builder.registry_client(self.brioche.registry_client.clone())
        })
        .await;

        let (projects, project_hash, _) = context.temp_project(f).await;
        let mocks = self
            .mock_registry_listing(&brioche, &projects, project_hash)
            .await;
        for mock in mocks {
            mock.create_async().await;
        }

        project_hash
    }

    #[must_use]
    pub fn mock_registry_publish_tag(
        &mut self,
        project_name: &str,
        tag: &str,
        project_hash: ProjectHash,
    ) -> mockito::Mock {
        self.registry_server
            .mock(
                "GET",
                &*format!(
                    "/v0/project-tags/{project_name}/{tag}?brioche={}",
                    brioche_core::VERSION
                ),
            )
            .with_header("Content-Type", "application/json")
            .with_body(
                serde_json::to_string(&brioche_core::registry::GetProjectTagResponse {
                    project_hash,
                })
                .unwrap(),
            )
    }

    #[must_use]
    pub async fn mock_registry_listing(
        &mut self,
        brioche: &Brioche,
        projects: &Projects,
        project_hash: ProjectHash,
    ) -> Vec<mockito::Mock> {
        let mut references = brioche_core::references::ProjectReferences::default();
        brioche_core::references::project_references(
            brioche,
            projects,
            &mut references,
            [project_hash],
        )
        .await
        .unwrap();

        let mut mocks = vec![];

        for (subproject_hash, subproject) in &references.projects {
            tracing::info!("mocking subproject {subproject_hash}");
            let mock = self
                .registry_server
                .mock(
                    "GET",
                    &*format!(
                        "/v0/projects/{subproject_hash}?brioche={}",
                        brioche_core::VERSION
                    ),
                )
                .with_header("Content-Type", "application/json")
                .with_body(serde_json::to_string(subproject).unwrap());

            mocks.push(mock);
        }
        for (blob_hash, blob_contents) in &references.loaded_blobs {
            let blob_contents_zstd = zstd::encode_all(&***blob_contents, 0).unwrap();
            let mock = self
                .registry_server
                .mock(
                    "GET",
                    &*format!(
                        "/v0/blobs/{blob_hash}.zst?brioche={}",
                        brioche_core::VERSION
                    ),
                )
                .with_header("Content-Type", "application/octet-stream")
                .with_body(blob_contents_zstd);

            mocks.push(mock);
        }
        for blob_hash in &references.recipes.blobs {
            let blob_path = brioche_core::blob::local_blob_path(brioche, *blob_hash);
            let blob_contents = tokio::fs::read(&blob_path).await.unwrap();
            let blob_contents_zstd = zstd::encode_all(&*blob_contents, 0).unwrap();
            let mock = self
                .registry_server
                .mock(
                    "GET",
                    &*format!(
                        "/v0/blobs/{blob_hash}.zst?brioche={}",
                        brioche_core::VERSION
                    ),
                )
                .with_header("Content-Type", "application/octet-stream")
                .with_body(blob_contents_zstd);

            mocks.push(mock);
        }
        for (recipe_hash, recipe) in &references.recipes.recipes {
            let recipe_json = serde_json::to_string(recipe).unwrap();
            let mock = self
                .registry_server
                .mock(
                    "GET",
                    &*format!(
                        "/v0/recipes/{recipe_hash}?brioche={}",
                        brioche_core::VERSION
                    ),
                )
                .with_header("Content-Type", "application/json")
                .with_body(recipe_json);

            mocks.push(mock);
        }

        mocks
    }
}

pub async fn brioche_lsp_test() -> (Brioche, TestContext, LspContext) {
    brioche_lsp_test_with(|builder| builder).await
}

#[expect(clippy::print_stderr)]
pub async fn brioche_lsp_test_with(
    f: impl FnOnce(BriocheBuilder) -> BriocheBuilder + Clone + Send + Sync + 'static,
) -> (Brioche, TestContext, LspContext) {
    let (brioche, context) = brioche_test_with(|builder| {
        builder
            .registry_client(brioche_core::registry::RegistryClient::disabled())
            .cache_client(brioche_core::cache::CacheClient::default())
            .vfs(brioche_core::vfs::Vfs::mutable())
    })
    .await;

    let (null_reporter, _null_guard) = brioche_core::reporter::start_null_reporter();
    let brioche_data_dir = brioche.data_dir.clone();
    let registry_server_url: url::Url = context.registry_server.url().parse().unwrap();
    let remote_brioche_builder = move || {
        let null_reporter = null_reporter.clone();
        let brioche_data_dir = brioche_data_dir.clone();
        let registry_server_url = registry_server_url.clone();
        let f = f.clone();
        let remote_brioche_fut = async move {
            let remote_brioche_builder = brioche_core::BriocheBuilder::new(null_reporter.clone())
                .config(brioche_core::config::BriocheConfig::default())
                .data_dir(brioche_data_dir)
                .registry_client(brioche_core::registry::RegistryClient::new_with_client(
                    reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build(),
                    registry_server_url.clone(),
                    brioche_core::registry::RegistryAuthentication::Admin {
                        password: "admin".to_string(),
                    },
                ))
                .cache_client(brioche_core::cache::CacheClient::default())
                .self_exec_processes(false);
            let remote_brioche_builder = f(remote_brioche_builder);
            anyhow::Ok(remote_brioche_builder)
        };
        remote_brioche_fut.boxed()
    };

    let projects = brioche_core::project::Projects::default();
    let (service, socket) = tower_lsp::LspService::new({
        let brioche = brioche.clone();
        move |client| {
            futures::executor::block_on(async move {
                let lsp_server = brioche_core::script::lsp::BriocheLspServer::new(
                    brioche,
                    projects,
                    client,
                    Arc::new(remote_brioche_builder),
                )
                .await?;
                anyhow::Ok(lsp_server)
            })
            .expect("failed to build LSP")
        }
    });

    let (client_io, server_io) = tokio::io::duplex(4096);
    let (client_in, mut client_out) = tokio::io::split(client_io);
    let (server_in, server_out) = tokio::io::split(server_io);

    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let response_handlers: Arc<
        tokio::sync::Mutex<HashMap<JsonRpcId, tokio::sync::oneshot::Sender<JsonRpcMessage>>>,
    > = Default::default();
    let notification_tx = tokio::sync::broadcast::Sender::new(100);

    // Used to indicate that one of the LSP client tasks has failed,
    // meaning in-flight requests will not receive a response. This can
    // be used to ensure we don't wait forever for a response that
    // won't arrive
    let cancellation_token = tokio_util::sync::CancellationToken::new();

    tokio::spawn({
        async move {
            while let Some(message) = request_rx.recv().await {
                let content =
                    serde_json::to_string(&message).context("failed to serialize message")?;
                let content_length = content.len();
                let content_length =
                    u64::try_from(content_length).context("content length out of range")?;

                let protocol_message = format!("Content-Length: {content_length}\r\n\r\n{content}");
                client_out
                    .write_all(protocol_message.as_bytes())
                    .await
                    .context("failed to write message")?;
            }

            anyhow::Ok(())
        }
        .inspect_err({
            let cancellation_token = cancellation_token.clone();
            move |error| {
                eprintln!("LSP client_out task returned error: {error:#}");
                cancellation_token.cancel();
            }
        })
    });

    tokio::spawn({
        let response_handlers = response_handlers.clone();
        let notification_tx = notification_tx.clone();
        async move {
            let mut client_in = tokio::io::BufReader::new(client_in);

            loop {
                let mut line = String::new();
                let read_bytes = client_in.read_line(&mut line).await?;
                if read_bytes == 0 {
                    break;
                }

                let mut headers = vec![];

                // Read headers
                loop {
                    if line.trim().is_empty() {
                        break;
                    }

                    let Some((header, value)) = line.split_once(": ") else {
                        anyhow::bail!("invalid header line: {line:?}");
                    };
                    headers.push((header.to_string(), value.to_string()));

                    line.clear();
                    let read_bytes = client_in.read_line(&mut line).await?;
                    if read_bytes == 0 {
                        anyhow::bail!("unexpected end of stream")
                    };
                }

                // Get Content-Length header
                let content_length_value = headers
                    .iter()
                    .find_map(|(header, value)| {
                        if header.eq_ignore_ascii_case("Content-Length") {
                            Some(value)
                        } else {
                            None
                        }
                    })
                    .context("request did not include Content-Length header")?;
                let content_length_value: u64 =
                    content_length_value.trim_end().parse().with_context(|| {
                        format!("invalid value for Content-Length header: {content_length_value:?}")
                    })?;
                let content_length_value: usize = content_length_value
                    .try_into()
                    .context("Content-Length out of range")?;

                // Read body
                let mut content = bstr::BString::new(vec![0; content_length_value]);
                client_in
                    .read_exact(&mut content)
                    .await
                    .context("failed to read message body")?;

                let message: JsonRpcMessage =
                    serde_json::from_slice(&content).with_context(|| {
                        format!(
                            "failed to deserialize message body: {content:?}",
                        )
                    })?;

                if let Some(id) = &message.id {
                    // Received a response message, find the handler
                    // to send it to
                    let mut response_handler = response_handlers.lock().await.remove(id).with_context(|| format!("received LSP message, but no associated handler is listening for it: {content:?}"))?;

                    response_handler.send(message).map_err(|_| anyhow::anyhow!("failed to send message to channel: {content:?}"))?;
                } else if message.error.is_some() {
                    // Received an error message, but it has no request
                    // ID meaning it must be a notification from the server

                    anyhow::bail!("received LSP error notification: {content:?}");
                } else {
                    // Try to broadcast notification
                    let _ = notification_tx.send(message);
                }
            }

            anyhow::Ok(())
        }
        .inspect_err({
            let cancellation_token = cancellation_token.clone();
            move |error| {
                eprintln!("LSP client_in task returned error: {error:#}");
                cancellation_token.cancel();
            }
        })
    });

    tokio::spawn(tower_lsp::Server::new(server_in, server_out, socket).serve(service));

    let mut lsp = LspContext {
        request_tx,
        response_handlers,
        notification_tx,
        next_id: Arc::new(AtomicU32::new(1)),
        cancellation_token,

        // Use a temporary value for the initialization result
        initialize_result: Default::default(),
    };

    // Send initialize request to the LSP server before returning, and
    // save the initialization result
    lsp.initialize_result = lsp
        .request::<request::Initialize>(lsp_types::InitializeParams::default())
        .await;

    lsp.notify::<notification::Initialized>(lsp_types::InitializedParams {})
        .await;

    (brioche, context, lsp)
}

pub struct LspContext {
    request_tx: tokio::sync::mpsc::UnboundedSender<JsonRpcMessage>,
    response_handlers:
        Arc<tokio::sync::Mutex<HashMap<JsonRpcId, tokio::sync::oneshot::Sender<JsonRpcMessage>>>>,
    notification_tx: tokio::sync::broadcast::Sender<JsonRpcMessage>,
    next_id: Arc<AtomicU32>,
    cancellation_token: tokio_util::sync::CancellationToken,
    pub initialize_result: lsp_types::InitializeResult,
}

impl LspContext {
    pub async fn request<T>(&self, params: T::Params) -> T::Result
    where
        T: request::Request,
    {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id = JsonRpcId::Number(
            serde_json::Number::from_u128(id.into()).expect("failed to serialize ID"),
        );
        let message = JsonRpcMessage {
            jsonrpc: JsonRpcVersion::V2_0,
            id: Some(id.clone()),
            method: Some(T::METHOD.to_string()),
            params: Some(serde_json::to_value(params).expect("failed to serialize params")),
            result: None,
            error: None,
        };

        let (res_tx, res_rx) = tokio::sync::oneshot::channel();
        {
            self.response_handlers.lock().await.insert(id, res_tx);
        }

        self.request_tx
            .send(message)
            .expect("failed to send request message");

        let res = self
            .cancellation_token
            .run_until_cancelled(res_rx)
            .await
            .expect("request cancelled")
            .expect("failed to receive response");

        let result = match res.into_response_result() {
            Ok(result) => result,
            Err(error) => {
                panic!("LSP request for {} failed: {error:#?}", T::METHOD);
            }
        };

        let result: T::Result = serde_json::from_value(result)
            .with_context(|| format!("failed to deserialize LSP response for {}", T::METHOD))
            .unwrap();
        result
    }

    pub async fn notify<T>(&self, params: T::Params)
    where
        T: notification::Notification,
    {
        let message = JsonRpcMessage {
            jsonrpc: JsonRpcVersion::V2_0,
            id: None,
            method: Some(T::METHOD.to_string()),
            params: Some(serde_json::to_value(params).expect("failed to serialize params")),
            error: None,
            result: None,
        };

        self.request_tx
            .send(message)
            .expect("failed to send notification message");
    }

    pub fn subscribe_to_notifications<T>(
        &self,
    ) -> impl futures::stream::Stream<Item = anyhow::Result<T::Params>>
    where
        T: notification::Notification,
    {
        let notification_rx = self.notification_tx.subscribe();
        tokio_stream::wrappers::BroadcastStream::new(notification_rx)
            .map(anyhow::Ok)
            .try_filter_map(|notification| async {
                let notification = notification?;
                if notification.method.as_deref() == Some(T::METHOD) {
                    let Some(params) = notification.params else {
                        anyhow::bail!("JSON RPC notification message did not include params");
                    };
                    let params: T::Params = serde_json::from_value(params)?;
                    Ok(Some(params))
                } else {
                    Ok(None)
                }
            })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JsonRpcMessage {
    jsonrpc: JsonRpcVersion,

    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<JsonRpcId>,

    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

impl JsonRpcMessage {
    #[expect(clippy::print_stderr)]
    fn into_response_result(self) -> Result<serde_json::Value, JsonRpcError> {
        if let Some(error) = self.error {
            return Err(error);
        };

        if let Some(result) = self.result {
            return Ok(result);
        };

        eprintln!("warning: tried to parse JSON RPC message as a response, but both result and error were None");
        Err(JsonRpcError {
            code: 12345678,
            message: "Failed to parse JSON RPC message as a result".to_string(),
            data: None,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JsonRpcError {
    code: i128,
    message: String,
    data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum JsonRpcVersion {
    #[serde(rename = "2.0")]
    V2_0,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum JsonRpcId {
    Number(serde_json::Number),
    String(String),
}
