#![allow(unused)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use brioche::brioche::{
    blob::{BlobId, SaveBlobOptions},
    value::{Directory, File, LazyDirectory, WithMeta},
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

pub async fn blob(brioche: &Brioche, content: impl AsRef<[u8]> + std::marker::Unpin) -> BlobId {
    let mut cursor = std::io::Cursor::new(content);
    brioche::brioche::blob::save_blob(brioche, &mut cursor, SaveBlobOptions::default())
        .await
        .unwrap()
}

pub fn lazy_file(blob: BlobId, executable: bool) -> brioche::brioche::value::LazyValue {
    brioche::brioche::value::LazyValue::File {
        data: blob,
        executable,
        resources: LazyDirectory::default(),
    }
}

pub fn file(blob: BlobId, executable: bool) -> brioche::brioche::value::CompleteValue {
    brioche::brioche::value::CompleteValue::File(File {
        data: blob,
        executable,
        resources: Directory::default(),
    })
}

pub fn file_with_resources(
    blob: BlobId,
    executable: bool,
    resources: brioche::brioche::value::Directory,
) -> brioche::brioche::value::CompleteValue {
    brioche::brioche::value::CompleteValue::File(File {
        data: blob,
        executable,
        resources,
    })
}

pub fn lazy_dir<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche::brioche::value::LazyValue)>,
) -> WithMeta<brioche::brioche::value::LazyValue> {
    WithMeta::without_meta(brioche::brioche::value::LazyValue::Directory(
        LazyDirectory {
            entries: entries
                .into_iter()
                .map(|(k, v)| (k.as_ref().into(), WithMeta::without_meta(v)))
                .collect(),
        },
    ))
}

pub fn dir_value<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche::brioche::value::CompleteValue)>,
) -> brioche::brioche::value::Directory {
    let mut directory = Directory::default();
    for (k, v) in entries {
        directory
            .insert(k.as_ref(), WithMeta::without_meta(v))
            .expect("failed to insert into dir");
    }

    directory
}

pub fn dir<K: AsRef<[u8]>>(
    entries: impl IntoIterator<Item = (K, brioche::brioche::value::CompleteValue)>,
) -> brioche::brioche::value::CompleteValue {
    brioche::brioche::value::CompleteValue::Directory(dir_value(entries))
}

pub fn lazy_dir_empty() -> brioche::brioche::value::LazyValue {
    brioche::brioche::value::LazyValue::Directory(LazyDirectory::default())
}

pub fn dir_empty() -> brioche::brioche::value::CompleteValue {
    brioche::brioche::value::CompleteValue::Directory(Directory::default())
}

pub fn lazy_symlink(target: impl AsRef<[u8]>) -> brioche::brioche::value::LazyValue {
    brioche::brioche::value::LazyValue::Symlink {
        target: target.as_ref().into(),
    }
}

pub fn symlink(target: impl AsRef<[u8]>) -> brioche::brioche::value::CompleteValue {
    brioche::brioche::value::CompleteValue::Symlink {
        target: target.as_ref().into(),
    }
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
