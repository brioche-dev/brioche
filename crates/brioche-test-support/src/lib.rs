#![allow(unused)]

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Output,
    sync::OnceLock,
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
use tokio::{io::AsyncSeekExt as _, sync::Mutex};

pub async fn brioche_test() -> (Brioche, TestContext) {
    brioche_test_with(|builder| builder).await
}

pub async fn brioche_test_with(
    f: impl FnOnce(BriocheBuilder) -> BriocheBuilder,
) -> (Brioche, TestContext) {
    let temp = tempdir::TempDir::new("brioche-test").unwrap();
    let registry_server = mockito::Server::new_async().await;

    let brioche_home = temp.path().join("brioche-home");
    tokio::fs::create_dir_all(&brioche_home)
        .await
        .expect("failed to create brioche home");
    let brioche_home = tokio::fs::canonicalize(&brioche_home)
        .await
        .expect("failed to canonicalize brioche home path");

    let (reporter, reporter_guard) = brioche_core::reporter::start_test_reporter();
    let builder = BriocheBuilder::new(reporter)
        .config(brioche_core::config::BriocheConfig::default())
        .home(brioche_home)
        .registry_client(brioche_core::registry::RegistryClient::new_with_client(
            reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build(),
            registry_server.url().parse().unwrap(),
            brioche_core::registry::RegistryAuthentication::Admin {
                password: "admin".to_string(),
            },
        ))
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
            .mkdir(format!("brioche-home/projects/{project_hash}"))
            .await;
        tokio::fs::rename(&temp_project_path, &project_path)
            .await
            .expect("failed to rename temp project to final location");

        (project_hash, project_path)
    }

    pub async fn remote_registry_project<F, Fut>(&mut self, f: F) -> ProjectHash
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
