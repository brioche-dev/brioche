use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use assert_matches::assert_matches;
use brioche_core::{
    Brioche, BriocheBuilder,
    path::AbsolutePath,
    projects::{ProjectRef, ProjectSpecifier, hash::ProjectHash},
};
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
        .data_dir(&brioche_data_dir)
        .registry_url(registry_server.url().parse().unwrap());
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
    let project_dir = brioche_core::path::canonicalize_system_path(project_dir)
        .await
        .unwrap();
    let specifier = ProjectSpecifier::Path(project_dir);
    let mut refs = brioche_core::projects::load::load_projects(brioche, [specifier.clone()])
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
        contents: &brioche_core::projects::Lockfile,
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
        let mut refs = brioche_core::projects::load::load_projects(brioche, [specifier.clone()])
            .await
            .unwrap();
        let project_ref = refs.remove(&specifier).unwrap();

        let issues = brioche_core::projects::get_all_issues(brioche).await;
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

        let mut refs = brioche_core::projects::load::load_projects(brioche, [specifier.clone()])
            .await
            .unwrap();
        let project_ref = refs.remove(&specifier).unwrap();

        (project_ref, temp_project_path)
    }

    pub async fn local_registry_project(
        &self,
        f: impl AsyncFnOnce(PathBuf),
    ) -> (ProjectHash, PathBuf) {
        let (temp_brioche, _temp_context) = brioche_test().await;
        let (project_ref, temp_project_path) = self.temp_project(&temp_brioche, f).await;
        let project_hash = brioche_core::projects::hash::hash_project(&temp_brioche, project_ref)
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
                builder.cache_client(brioche_core::cache::CacheClient {
                    store: Some(cache),
                    writable: true,
                    ..Default::default()
                })
            }
        })
        .await;

        let (project_ref, _) = temp_context.temp_project_by_path(&temp_brioche, f).await;

        let project_hash = brioche_core::projects::hash::hash_project(&temp_brioche, project_ref)
            .await
            .unwrap();
        let project_artifact =
            brioche_core::projects::artifact::create_project_artifact(&temp_brioche, project_ref)
                .await
                .expect("failed to create artifact for project");
        let project_artifact_hash =
            brioche_core::recipe::hash::hash_recipe(&temp_brioche, project_artifact).await;
        brioche_core::cache::save_artifact(&temp_brioche, project_artifact)
            .await
            .expect("failed to save artifact to cache");
        brioche_core::cache::save_project_artifact_hash(
            &temp_brioche,
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
