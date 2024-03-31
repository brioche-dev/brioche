#![allow(unused)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use brioche::brioche::{
    artifact::{CreateDirectory, Directory, DirectoryListing, File, WithMeta},
    blob::{BlobId, SaveBlobOptions},
    project::{ProjectHash, Projects},
    Brioche, BriocheBuilder,
};

pub async fn brioche_test() -> (Brioche, TestContext) {
    let temp = tempdir::TempDir::new("brioche-test").unwrap();

    let brioche_home = temp.path().join("brioche-home");
    let brioche_repo = temp.path().join("brioche-repo");
    tokio::fs::create_dir_all(&brioche_home)
        .await
        .expect("failed to create brioche home");
    tokio::fs::create_dir_all(&brioche_repo)
        .await
        .expect("failed to create brioche repo");

    let (reporter, reporter_guard) = brioche::reporter::start_test_reporter();
    let brioche = BriocheBuilder::new(reporter)
        .home(brioche_home)
        .repo_dir(brioche_repo)
        .self_exec_processes(false)
        .build()
        .await
        .unwrap();
    let context = TestContext {
        temp,
        _reporter_guard: reporter_guard,
    };
    (brioche, context)
}

pub async fn load_project(
    brioche: &Brioche,
    path: &Path,
) -> anyhow::Result<(Projects, ProjectHash)> {
    let projects = Projects::default();
    let project_hash = projects.load(brioche, path).await?;

    Ok((projects, project_hash))
}

pub async fn resolve_without_meta(
    brioche: &Brioche,
    artifact: brioche::brioche::artifact::LazyArtifact,
) -> anyhow::Result<brioche::brioche::artifact::CompleteArtifact> {
    let resolved = brioche::brioche::resolve::resolve(brioche, without_meta(artifact)).await?;
    Ok(resolved.value)
}

pub async fn blob(brioche: &Brioche, content: impl AsRef<[u8]> + std::marker::Unpin) -> BlobId {
    brioche::brioche::blob::save_blob_from_reader(
        brioche,
        content.as_ref(),
        SaveBlobOptions::default(),
    )
    .await
    .unwrap()
}

pub fn lazy_file(blob: BlobId, executable: bool) -> brioche::brioche::artifact::LazyArtifact {
    brioche::brioche::artifact::LazyArtifact::File {
        content_blob: blob,
        executable,
        resources: Box::new(WithMeta::without_meta(
            brioche::brioche::artifact::LazyArtifact::Directory(Directory::default()),
        )),
    }
}

pub fn lazy_file_with_resources(
    blob: BlobId,
    executable: bool,
    resources: brioche::brioche::artifact::LazyArtifact,
) -> brioche::brioche::artifact::LazyArtifact {
    brioche::brioche::artifact::LazyArtifact::File {
        content_blob: blob,
        executable,
        resources: Box::new(WithMeta::without_meta(resources)),
    }
}

pub fn file(blob: BlobId, executable: bool) -> brioche::brioche::artifact::CompleteArtifact {
    brioche::brioche::artifact::CompleteArtifact::File(File {
        content_blob: blob,
        executable,
        resources: Directory::default(),
    })
}

pub fn file_with_resources(
    blob: BlobId,
    executable: bool,
    resources: brioche::brioche::artifact::Directory,
) -> brioche::brioche::artifact::CompleteArtifact {
    brioche::brioche::artifact::CompleteArtifact::File(File {
        content_blob: blob,
        executable,
        resources,
    })
}

pub fn lazy_dir_value<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche::brioche::artifact::LazyArtifact)>,
) -> brioche::brioche::artifact::CreateDirectory {
    CreateDirectory {
        entries: entries
            .into_iter()
            .map(|(k, v)| (k.as_ref().into(), without_meta(v)))
            .collect(),
    }
}

pub fn lazy_dir<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche::brioche::artifact::LazyArtifact)>,
) -> brioche::brioche::artifact::LazyArtifact {
    brioche::brioche::artifact::LazyArtifact::CreateDirectory(CreateDirectory {
        entries: entries
            .into_iter()
            .map(|(k, v)| (k.as_ref().into(), WithMeta::without_meta(v)))
            .collect(),
    })
}

pub fn empty_dir_value() -> brioche::brioche::artifact::Directory {
    brioche::brioche::artifact::Directory::default()
}

pub async fn dir_value<K: AsRef<[u8]>>(
    brioche: &Brioche,
    entries: impl IntoIterator<Item = (K, brioche::brioche::artifact::CompleteArtifact)>,
) -> brioche::brioche::artifact::Directory {
    let mut listing = DirectoryListing::default();
    for (k, v) in entries {
        listing
            .insert(brioche, k.as_ref(), Some(WithMeta::without_meta(v)))
            .await
            .expect("failed to insert into dir");
    }

    Directory::create(brioche, &listing)
        .await
        .expect("failed to create dir")
}

pub async fn dir<K: AsRef<[u8]>>(
    brioche: &Brioche,
    entries: impl IntoIterator<Item = (K, brioche::brioche::artifact::CompleteArtifact)>,
) -> brioche::brioche::artifact::CompleteArtifact {
    brioche::brioche::artifact::CompleteArtifact::Directory(dir_value(brioche, entries).await)
}

pub fn lazy_dir_empty() -> brioche::brioche::artifact::LazyArtifact {
    brioche::brioche::artifact::LazyArtifact::CreateDirectory(CreateDirectory::default())
}

pub fn dir_empty() -> brioche::brioche::artifact::CompleteArtifact {
    brioche::brioche::artifact::CompleteArtifact::Directory(Directory::default())
}

pub fn lazy_symlink(target: impl AsRef<[u8]>) -> brioche::brioche::artifact::LazyArtifact {
    brioche::brioche::artifact::LazyArtifact::Symlink {
        target: target.as_ref().into(),
    }
}

pub fn symlink(target: impl AsRef<[u8]>) -> brioche::brioche::artifact::CompleteArtifact {
    brioche::brioche::artifact::CompleteArtifact::Symlink {
        target: target.as_ref().into(),
    }
}

pub fn without_meta<T>(value: T) -> WithMeta<T> {
    WithMeta::without_meta(value)
}

pub fn sha256(value: impl AsRef<[u8]>) -> brioche::brioche::Hash {
    let mut hasher = brioche::brioche::Hasher::Sha256(Default::default());
    hasher.update(value.as_ref());
    hasher.finish().unwrap()
}

pub struct TestContext {
    temp: tempdir::TempDir,
    _reporter_guard: brioche::reporter::ReporterGuard,
}

impl TestContext {
    pub fn path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.temp.path().join(path)
    }

    pub async fn mkdir(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = self.temp.path().join(path);
        tokio::fs::create_dir_all(&path).await.unwrap();
        path
    }

    pub async fn write_file(&self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.temp.path().join(path);

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }

        tokio::fs::write(&path, contents.as_ref()).await.unwrap();

        path
    }

    pub async fn write_symlink(&self, src: impl AsRef<Path>, dst: impl AsRef<Path>) -> PathBuf {
        let dst = self.temp.path().join(dst);

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
}
