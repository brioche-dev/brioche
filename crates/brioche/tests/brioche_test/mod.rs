#![allow(unused)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Output,
};

use brioche::{
    blob::{BlobHash, SaveBlobOptions},
    project::{self, ProjectHash, ProjectListing, Projects},
    recipe::{CreateDirectory, Directory, File, WithMeta},
    Brioche, BriocheBuilder,
};

pub async fn brioche_test() -> (Brioche, TestContext) {
    brioche_test_with(|builder| builder).await
}

pub async fn brioche_test_with(
    f: impl FnOnce(BriocheBuilder) -> BriocheBuilder,
) -> (Brioche, TestContext) {
    let temp = tempdir::TempDir::new("brioche-test").unwrap();
    let registry_server = mockito::Server::new();

    let brioche_home = temp.path().join("brioche-home");
    tokio::fs::create_dir_all(&brioche_home)
        .await
        .expect("failed to create brioche home");
    let brioche_home = tokio::fs::canonicalize(&brioche_home)
        .await
        .expect("failed to canonicalize brioche home path");

    let (reporter, reporter_guard) = brioche::reporter::start_test_reporter();
    let builder = BriocheBuilder::new(reporter)
        .home(brioche_home)
        .registry_client(brioche::registry::RegistryClient::new(
            registry_server.url().parse().unwrap(),
            brioche::registry::RegistryAuthentication::Admin {
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

pub async fn load_project(
    brioche: &Brioche,
    path: &Path,
) -> anyhow::Result<(Projects, ProjectHash)> {
    let projects = Projects::default();
    let project_hash = projects.load(brioche, path, true).await?;

    Ok((projects, project_hash))
}

pub async fn load_project_no_validate(
    brioche: &Brioche,
    path: &Path,
) -> anyhow::Result<(Projects, ProjectHash)> {
    let projects = Projects::default();
    let project_hash = projects.load(brioche, path, false).await?;

    Ok((projects, project_hash))
}

pub async fn bake_without_meta(
    brioche: &Brioche,
    recipe: brioche::recipe::Recipe,
) -> anyhow::Result<brioche::recipe::Artifact> {
    let artifact = brioche::bake::bake(
        brioche,
        without_meta(recipe),
        &brioche::bake::BakeScope::Anonymous,
    )
    .await?;
    Ok(artifact.value)
}

pub async fn blob(brioche: &Brioche, content: impl AsRef<[u8]> + std::marker::Unpin) -> BlobHash {
    brioche::blob::save_blob_from_reader(brioche, content.as_ref(), SaveBlobOptions::default())
        .await
        .unwrap()
}

pub fn lazy_file(blob: BlobHash, executable: bool) -> brioche::recipe::Recipe {
    brioche::recipe::Recipe::File {
        content_blob: blob,
        executable,
        resources: Box::new(WithMeta::without_meta(brioche::recipe::Recipe::Directory(
            Directory::default(),
        ))),
    }
}

pub fn lazy_file_with_resources(
    blob: BlobHash,
    executable: bool,
    resources: brioche::recipe::Recipe,
) -> brioche::recipe::Recipe {
    brioche::recipe::Recipe::File {
        content_blob: blob,
        executable,
        resources: Box::new(WithMeta::without_meta(resources)),
    }
}

pub fn file(blob: BlobHash, executable: bool) -> brioche::recipe::Artifact {
    brioche::recipe::Artifact::File(File {
        content_blob: blob,
        executable,
        resources: Directory::default(),
    })
}

pub fn file_with_resources(
    blob: BlobHash,
    executable: bool,
    resources: brioche::recipe::Directory,
) -> brioche::recipe::Artifact {
    brioche::recipe::Artifact::File(File {
        content_blob: blob,
        executable,
        resources,
    })
}

pub fn lazy_dir_value<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche::recipe::Recipe)>,
) -> brioche::recipe::CreateDirectory {
    CreateDirectory {
        entries: entries
            .into_iter()
            .map(|(k, v)| (k.as_ref().into(), without_meta(v)))
            .collect(),
    }
}

pub fn lazy_dir<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche::recipe::Recipe)>,
) -> brioche::recipe::Recipe {
    brioche::recipe::Recipe::CreateDirectory(CreateDirectory {
        entries: entries
            .into_iter()
            .map(|(k, v)| (k.as_ref().into(), WithMeta::without_meta(v)))
            .collect(),
    })
}

pub fn empty_dir_value() -> brioche::recipe::Directory {
    brioche::recipe::Directory::default()
}

pub async fn dir_value<K: AsRef<[u8]>>(
    brioche: &Brioche,
    entries: impl IntoIterator<Item = (K, brioche::recipe::Artifact)>,
) -> brioche::recipe::Directory {
    let mut directory = Directory::default();
    for (k, v) in entries {
        directory
            .insert(brioche, k.as_ref(), Some(WithMeta::without_meta(v)))
            .await
            .expect("failed to insert into dir");
    }

    directory
}

pub async fn dir<K: AsRef<[u8]>>(
    brioche: &Brioche,
    entries: impl IntoIterator<Item = (K, brioche::recipe::Artifact)>,
) -> brioche::recipe::Artifact {
    brioche::recipe::Artifact::Directory(dir_value(brioche, entries).await)
}

pub fn lazy_dir_empty() -> brioche::recipe::Recipe {
    brioche::recipe::Recipe::CreateDirectory(CreateDirectory::default())
}

pub fn dir_empty() -> brioche::recipe::Artifact {
    brioche::recipe::Artifact::Directory(Directory::default())
}

pub fn lazy_symlink(target: impl AsRef<[u8]>) -> brioche::recipe::Recipe {
    brioche::recipe::Recipe::Symlink {
        target: target.as_ref().into(),
    }
}

pub fn symlink(target: impl AsRef<[u8]>) -> brioche::recipe::Artifact {
    brioche::recipe::Artifact::Symlink {
        target: target.as_ref().into(),
    }
}

pub fn without_meta<T>(value: T) -> WithMeta<T> {
    WithMeta::without_meta(value)
}

pub fn sha256(value: impl AsRef<[u8]>) -> brioche::Hash {
    let mut hasher = brioche::Hasher::Sha256(Default::default());
    hasher.update(value.as_ref());
    hasher.finish().unwrap()
}

pub struct TestContext {
    brioche: Brioche,
    temp: tempdir::TempDir,
    pub registry_server: mockito::ServerGuard,
    _reporter_guard: brioche::reporter::ReporterGuard,
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
            .load(&self.brioche, &temp_project_path, true)
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
        let (projects, project_hash, _) = self.temp_project(f).await;
        let project_listing = projects
            .export_listing(&self.brioche, project_hash)
            .expect("failed to export project listing");

        let mocks = self.mock_registry_listing(&project_listing);
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
            .mock("GET", &*format!("/v0/project-tags/{project_name}/{tag}"))
            .with_header("Content-Type", "application/json")
            .with_body(
                serde_json::to_string(&brioche::registry::GetProjectTagResponse { project_hash })
                    .unwrap(),
            )
    }

    #[must_use]
    pub fn mock_registry_listing(
        &mut self,
        project_listing: &ProjectListing,
    ) -> Vec<mockito::Mock> {
        let mut mocks = vec![];

        for (subproject_hash, subproject) in &project_listing.projects {
            let mock = self
                .registry_server
                .mock("GET", &*format!("/v0/projects/{subproject_hash}"))
                .with_header("Content-Type", "application/json")
                .with_body(serde_json::to_string(subproject).unwrap());

            mocks.push(mock);
        }
        for (file_id, file_contents) in &project_listing.files {
            let file_contents_zstd = zstd::encode_all(&**file_contents, 0).unwrap();
            let mock = self
                .registry_server
                .mock("GET", &*format!("/v0/blobs/{file_id}.zst"))
                .with_header("Content-Type", "application/octet-stream")
                .with_body(file_contents_zstd);

            mocks.push(mock);
        }

        mocks
    }
}
