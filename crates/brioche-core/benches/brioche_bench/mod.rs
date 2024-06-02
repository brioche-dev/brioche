#![allow(unused)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use brioche_core::{
    blob::{BlobHash, SaveBlobOptions},
    recipe::{CreateDirectory, Directory, File, WithMeta},
    Brioche, BriocheBuilder,
};

pub async fn brioche_test() -> (Brioche, TestContext) {
    let temp = tempdir::TempDir::new("brioche-test").unwrap();

    let brioche_home = temp.path().join("brioche-home");
    tokio::fs::create_dir_all(&brioche_home)
        .await
        .expect("failed to create brioche home");

    let (reporter, reporter_guard) = brioche_core::reporter::start_test_reporter();
    let brioche = BriocheBuilder::new(reporter)
        .home(brioche_home)
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

pub async fn blob(brioche: &Brioche, content: impl AsRef<[u8]> + std::marker::Unpin) -> BlobHash {
    let permit = brioche_core::blob::get_save_blob_permit().await.unwrap();
    brioche_core::blob::save_blob(
        brioche,
        permit,
        content.as_ref(),
        SaveBlobOptions::default(),
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

pub fn lazy_dir<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche_core::recipe::Recipe)>,
) -> WithMeta<brioche_core::recipe::Recipe> {
    WithMeta::without_meta(brioche_core::recipe::Recipe::CreateDirectory(
        CreateDirectory {
            entries: entries
                .into_iter()
                .map(|(k, v)| (k.as_ref().into(), WithMeta::without_meta(v)))
                .collect(),
        },
    ))
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
            .insert(brioche, k.as_ref(), Some(WithMeta::without_meta(v)))
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

pub fn sha256(value: impl AsRef<[u8]>) -> brioche_core::Hash {
    let mut hasher = brioche_core::Hasher::Sha256(Default::default());
    hasher.update(value.as_ref());
    hasher.finish().unwrap()
}

pub struct TestContext {
    temp: tempdir::TempDir,
    _reporter_guard: brioche_core::reporter::ReporterGuard,
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
