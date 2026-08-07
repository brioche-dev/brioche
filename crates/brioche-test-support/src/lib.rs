use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use assert_matches::assert_matches;
use brioche_core::{
    Brioche, BriocheBuilder,
    blob::{BlobHash, SaveBlobOptions},
    path::AbsolutePath,
    project::{ProjectRef, ProjectSpecifier, hash::ProjectHash},
    recipe::RecipeRef,
};
use bstr::ByteSlice as _;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

pub async fn brioche_test() -> (Brioche, TestContext) {
    brioche_test_with(|builder| builder).await
}

pub async fn brioche_test_with_cache(
    cache: Arc<dyn object_store::ObjectStore>,
    writable: bool,
) -> (Brioche, TestContext) {
    brioche_test_with(|builder| {
        builder.cache_client(brioche_core::cache::CacheClient {
            store: Some(cache),
            writable,
            ..Default::default()
        })
    })
    .await
}

pub async fn brioche_test_with(
    f: impl FnOnce(BriocheBuilder) -> BriocheBuilder,
) -> (Brioche, TestContext) {
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_target(false)
                .without_time(),
        )
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("brioche=info,warn")),
        )
        .try_init();
    let (reporter, reporter_guard) = brioche_core::reporter::start_null_reporter();

    let temp = tempfile::TempDir::with_prefix("brioche-test").unwrap();
    let registry_server = mockito::Server::new_async().await;

    let brioche_data_dir = temp.path().join("brioche-data");
    tokio::fs::create_dir_all(&brioche_data_dir)
        .await
        .expect("failed to create brioche data dir");

    let builder = Brioche::builder()
        .reporter(reporter)
        .config(brioche_core::config::BriocheConfig::default())
        .cache_client(brioche_core::cache::CacheClient::default())
        .registry_client(test_registry_client(&registry_server.url()))
        .data_dir(&brioche_data_dir);
    let builder = f(builder);
    let brioche = builder.build().await.unwrap();
    let context = TestContext {
        temp,
        registry_server,
        _reporter_guard: reporter_guard,
    };
    (brioche, context)
}

pub async fn load_project(brioche: &Brioche, project_dir: &Path) -> ProjectRef {
    let mut brioche = brioche.write().await;

    let project_dir = brioche_core::path::canonicalize_system_path(project_dir)
        .await
        .unwrap();
    let specifier = ProjectSpecifier::Path(project_dir);
    let mut refs = brioche_core::project::load::load_projects(&mut brioche, [specifier.clone()])
        .await
        .unwrap();
    brioche_core::project::load::resolve_statics(&mut brioche)
        .await
        .unwrap();
    refs.remove(&specifier).unwrap()
}

#[must_use]
pub fn absolute_path(path: &Path) -> AbsolutePath {
    let path = std::fs::canonicalize(path).unwrap();
    brioche_core::path::from_canonical_system_path(&path).unwrap()
}

#[must_use]
pub fn absolute_path_nonexistent(path: &Path) -> AbsolutePath {
    brioche_core::path::from_canonical_system_path(path).unwrap()
}

#[must_use]
pub fn project_specifier_for_path(project_dir: &Path) -> ProjectSpecifier {
    ProjectSpecifier::Path(absolute_path(project_dir))
}

pub fn take_where<T>(items: &mut Vec<T>, mut predicate: impl FnMut(&T) -> bool) -> T {
    let index = items
        .iter()
        .enumerate()
        .find_map(|(index, item)| if predicate(item) { Some(index) } else { None })
        .expect("no item found matching predicate");
    items.remove(index)
}

#[must_use]
pub fn new_cache() -> Arc<dyn object_store::ObjectStore> {
    Arc::new(object_store::memory::InMemory::new())
}

pub async fn get_recipe_within(
    brioche: &Brioche,
    mut recipe_ref: RecipeRef,
    path: impl AsRef<[u8]>,
) -> RecipeRef {
    let brioche = brioche.read().await;

    let path_components = path
        .as_ref()
        .split_str(b"/")
        .filter(|component| !component.is_empty());

    for path_component in path_components {
        let path_component = bstr::BStr::new(path_component);
        let recipe = brioche_core::recipe::get_recipe(&brioche, recipe_ref);
        let brioche_core::recipe::Recipe::Directory(directory) = &*recipe else {
            panic!(
                "tried to traverse into subpath '{path_component}' into non-directory recipe ({:?})",
                recipe.kind()
            );
        };

        recipe_ref = *directory
            .entries
            .get(path_component)
            .unwrap_or_else(|| panic!("directory does not contain subpath '{path_component}'"));
    }

    recipe_ref
}

pub async fn read_file_recipe_content(brioche: &Brioche, recipe_ref: RecipeRef) -> Vec<u8> {
    let brioche = brioche.read().await;

    let recipe = brioche_core::recipe::get_recipe(&brioche, recipe_ref);
    let brioche_core::recipe::Recipe::File(file) = &*recipe else {
        panic!("expected recipe to be a file, was {:?}", recipe.kind());
    };

    let blob_path = brioche_core::blob::local_blob_path(brioche.resources(), file.content_blob);
    tokio::fs::read(&blob_path)
        .await
        .expect("failed to read file blob")
}

pub async fn read_file_within(
    brioche: &Brioche,
    mut recipe_ref: RecipeRef,
    path: impl AsRef<[u8]>,
) -> RecipeRef {
    let brioche = brioche.read().await;

    let path_components = path
        .as_ref()
        .split_str(b"/")
        .filter(|component| !component.is_empty());

    for path_component in path_components {
        let path_component = bstr::BStr::new(path_component);
        let recipe = brioche_core::recipe::get_recipe(&brioche, recipe_ref);
        let brioche_core::recipe::Recipe::Directory(directory) = &*recipe else {
            panic!(
                "tried to traverse into subpath '{path_component}' into non-directory recipe ({:?})",
                recipe.kind()
            );
        };

        recipe_ref = *directory
            .entries
            .get(path_component)
            .unwrap_or_else(|| panic!("directory does not contain subpath '{path_component}'"));
    }

    recipe_ref
}

pub fn artifact_path(path: impl AsRef<[u8]>) -> brioche_core::recipe::build::ArtifactPath {
    let components = path
        .as_ref()
        .split_str(b"/")
        .filter(|component| !component.is_empty())
        .map(|component| {
            brioche_core::recipe::build::ArtifactPathComponent::DirectoryEntry(component.into())
        })
        .collect();

    brioche_core::recipe::build::ArtifactPath { components }
}

pub async fn blob(brioche: &Brioche, content: impl AsRef<[u8]>) -> BlobHash {
    brioche_core::blob::save_blob(
        brioche.resources(),
        &mut brioche_core::blob::get_save_blob_permit().await,
        content.as_ref(),
        SaveBlobOptions::default(),
    )
    .await
    .unwrap()
}

fn test_registry_client(url: &str) -> brioche_core::registry::RegistryClient {
    brioche_core::registry::RegistryClient::new(brioche_core::registry::RegistryClientConfig {
        url: url.parse().expect("invalid registry URL"),
        retry: false,
    })
}

pub struct TestContext {
    temp: tempfile::TempDir,
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

    pub async fn temp_project(
        &self,
        brioche: &Brioche,
        f: impl AsyncFnOnce(PathBuf),
    ) -> (ProjectRef, PathBuf) {
        let temp_project_path = self
            .mkdir(format!("temp-project-{}", ulid::Ulid::new()))
            .await;

        f(temp_project_path.clone()).await;

        let project_dir = brioche_core::path::canonicalize_system_path(&temp_project_path)
            .await
            .unwrap();
        let specifier = ProjectSpecifier::Path(project_dir);
        let mut refs = brioche_core::project::load::load_projects(
            &mut *brioche.write().await,
            [specifier.clone()],
        )
        .await
        .unwrap();
        let project_ref = refs.remove(&specifier).unwrap();

        let issues = brioche_core::project::get_all_issues(&*brioche.read().await);
        assert_matches!(&issues[..], []);

        (project_ref, temp_project_path)
    }

    pub async fn temp_project_by_path(
        &self,
        brioche: &Brioche,
        f: impl AsyncFnOnce(&Self) -> PathBuf,
    ) -> (ProjectRef, PathBuf) {
        let temp_project_path = f(self).await;

        let project_dir = brioche_core::path::canonicalize_system_path(&temp_project_path)
            .await
            .unwrap();
        let specifier = ProjectSpecifier::Path(project_dir);

        let mut refs = brioche_core::project::load::load_projects(
            &mut *brioche.write().await,
            [specifier.clone()],
        )
        .await
        .expect("failed to load temp project");
        let project_ref = refs.remove(&specifier).unwrap();

        brioche_core::project::load::resolve_statics(&mut *brioche.write().await)
            .await
            .expect("failed to resolve temp project statics");

        (project_ref, temp_project_path)
    }

    pub async fn local_registry_project(
        &self,
        f: impl AsyncFnOnce(PathBuf),
    ) -> (ProjectHash, PathBuf) {
        // Create a temporary test context so the project does not get
        // loaded into the current context
        let (temp_brioche, temp_context) = brioche_test_with({
            |builder| {
                builder
                    .registry_client(test_registry_client(&self.registry_server.url()))
                    .data_dir(self.temp.path().join("brioche-data"))
            }
        })
        .await;

        let (project_ref, temp_project_path) = temp_context.temp_project(&temp_brioche, f).await;
        let project_hash = brioche_core::project::hash::hash_project(
            &mut *temp_brioche.write().await,
            project_ref,
        )
        .await
        .unwrap();

        let project_path = self
            .mkdir(format!("brioche-data/projects/{project_hash}"))
            .await;
        tokio::fs::rename(&temp_project_path, &project_path)
            .await
            .expect("failed to rename temp project to final location");

        (project_hash, project_path)
    }

    pub async fn cached_registry_project(
        &mut self,
        cache: &Arc<dyn object_store::ObjectStore>,
        f: impl AsyncFnOnce(PathBuf),
    ) -> ProjectHash {
        self.cached_registry_project_by_path(cache, async |context| {
            let temp_project_path = context
                .mkdir(format!("temp-project-{}", ulid::Ulid::new()))
                .await;
            f(temp_project_path.clone()).await;
            temp_project_path
        })
        .await
    }

    pub async fn cached_registry_project_by_path(
        &mut self,
        cache: &Arc<dyn object_store::ObjectStore>,
        f: impl AsyncFnOnce(&Self) -> PathBuf,
    ) -> ProjectHash {
        // Create a temporary test context so the project does not get
        // loaded into the current context
        let (temp_brioche, temp_context) = brioche_test_with({
            let cache = cache.clone();
            |builder| {
                builder
                    .registry_client(test_registry_client(&self.registry_server.url()))
                    .cache_client(brioche_core::cache::CacheClient {
                        store: Some(cache),
                        writable: true,
                        ..Default::default()
                    })
            }
        })
        .await;

        let (project_ref, _) = temp_context.temp_project_by_path(&temp_brioche, f).await;

        let project_hash = brioche_core::project::hash::hash_project(
            &mut *temp_brioche.write().await,
            project_ref,
        )
        .await
        .unwrap();
        let project_artifact = brioche_core::project::artifact::create_project_artifact(
            &mut *temp_brioche.write().await,
            project_ref,
        )
        .await
        .expect("failed to create artifact for project");
        let project_artifact_hash = brioche_core::recipe::hash::hash_recipe(
            &mut *temp_brioche.write().await,
            project_artifact,
        );
        brioche_core::cache::save_artifact(&mut *temp_brioche.write().await, project_artifact)
            .await
            .expect("failed to save artifact to cache");
        brioche_core::cache::save_project_artifact_hash(
            &mut *temp_brioche.write().await,
            project_hash,
            project_artifact_hash,
        )
        .await
        .expect("failed to save project artifact hash to cache");

        project_hash
    }

    #[must_use]
    pub fn mock_registry_publish_tag(
        &mut self,
        project_name: &str,
        tag: &str,
        project_hash: ProjectHash,
    ) -> mockito::Mock {
        self.mock_registry_tag_response(project_name, tag)
            .with_header("Content-Type", "application/json")
            .with_body(
                serde_json::to_string(&brioche_core::registry::GetProjectTagResponse {
                    project_hash,
                })
                .unwrap(),
            )
    }

    #[must_use]
    pub fn mock_registry_tag_response(&mut self, project_name: &str, tag: &str) -> mockito::Mock {
        self.registry_server.mock(
            "GET",
            &*format!(
                "/v0/project-tags/{project_name}/{tag}?brioche={}",
                brioche_core::VERSION
            ),
        )
    }

    // #[must_use]
    // pub async fn mock_registry_listing(
    //     &mut self,
    //     brioche: &Brioche,
    //     projects: &Projects,
    //     project_hash: ProjectHash,
    // ) -> Vec<mockito::Mock> {
    //     let mut references = brioche_core::references::ProjectReferences::default();
    //     brioche_core::references::project_references(
    //         brioche,
    //         projects,
    //         &mut references,
    //         [project_hash],
    //     )
    //     .await
    //     .unwrap();

    //     let mut mocks = vec![];

    //     for (subproject_hash, subproject) in &references.projects {
    //         tracing::info!("mocking subproject {subproject_hash}");
    //         let mock = self
    //             .registry_server
    //             .mock(
    //                 "GET",
    //                 &*format!(
    //                     "/v0/projects/{subproject_hash}?brioche={}",
    //                     brioche_core::VERSION
    //                 ),
    //             )
    //             .with_header("Content-Type", "application/json")
    //             .with_body(serde_json::to_string(subproject).unwrap());

    //         mocks.push(mock);
    //     }
    //     for (blob_hash, blob_contents) in &references.loaded_blobs {
    //         let blob_contents_zstd = zstd::encode_all(&***blob_contents, 0).unwrap();
    //         let mock = self
    //             .registry_server
    //             .mock(
    //                 "GET",
    //                 &*format!(
    //                     "/v0/blobs/{blob_hash}.zst?brioche={}",
    //                     brioche_core::VERSION
    //                 ),
    //             )
    //             .with_header("Content-Type", "application/octet-stream")
    //             .with_body(blob_contents_zstd);

    //         mocks.push(mock);
    //     }
    //     for blob_hash in &references.recipes.blobs {
    //         let blob_path = brioche_core::blob::local_blob_path(brioche, *blob_hash);
    //         let blob_contents = tokio::fs::read(&blob_path).await.unwrap();
    //         let blob_contents_zstd = zstd::encode_all(&*blob_contents, 0).unwrap();
    //         let mock = self
    //             .registry_server
    //             .mock(
    //                 "GET",
    //                 &*format!(
    //                     "/v0/blobs/{blob_hash}.zst?brioche={}",
    //                     brioche_core::VERSION
    //                 ),
    //             )
    //             .with_header("Content-Type", "application/octet-stream")
    //             .with_body(blob_contents_zstd);

    //         mocks.push(mock);
    //     }
    //     for (recipe_hash, recipe) in &references.recipes.recipes {
    //         let recipe_json = serde_json::to_string(recipe).unwrap();
    //         let mock = self
    //             .registry_server
    //             .mock(
    //                 "GET",
    //                 &*format!(
    //                     "/v0/recipes/{recipe_hash}?brioche={}",
    //                     brioche_core::VERSION
    //                 ),
    //             )
    //             .with_header("Content-Type", "application/json")
    //             .with_body(recipe_json);

    //         mocks.push(mock);
    //     }

    //     mocks
    // }
}
