use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default)]
pub struct Vfs {
    inner: Arc<RwLock<Arc<VfsInner>>>,
}

impl Vfs {
    pub async fn load(&self, path: &Path) -> anyhow::Result<(FileId, Arc<Vec<u8>>)> {
        let path = crate::fs_utils::logical_path(path);

        {
            let vfs = self.inner.read().await;
            if let Some(file_id) = vfs.locations_to_ids.get(&path) {
                let contents = vfs.contents[file_id].clone();
                return Ok((*file_id, contents));
            }
        }

        let contents = tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read file {}", path.display()))?;
        let contents = Arc::new(contents);

        let hash = blake3::hash(&contents);
        let file_id = FileId(hash);

        let mut vfs = self.inner.write().await;
        let vfs = Arc::make_mut(&mut vfs);
        vfs.contents.insert(file_id, contents.clone());
        vfs.locations_to_ids.insert(path.clone(), file_id);
        vfs.ids_to_locations
            .entry(file_id)
            .or_default()
            .push(path.clone());

        tracing::debug!(path = %path.display(), %file_id, "loaded file into VFS");

        Ok((file_id, contents))
    }

    pub async fn read(&self, file_id: FileId) -> Option<Arc<Vec<u8>>> {
        let vfs = self.inner.read().await;
        vfs.contents.get(&file_id).cloned()
    }

    pub async fn snapshot(&self) -> VfsSnapshot {
        let inner = self.inner.read().await.clone();
        VfsSnapshot { inner }
    }
}

pub struct VfsSnapshot {
    inner: Arc<VfsInner>,
}

impl VfsSnapshot {
    pub fn load_cached(&self, path: &Path) -> Option<(FileId, Arc<Vec<u8>>)> {
        let path = crate::fs_utils::logical_path(path);
        let file_id = self.inner.locations_to_ids.get(&path)?;
        let contents = self.inner.contents.get(file_id)?.clone();
        Some((*file_id, contents))
    }

    pub fn read(&self, file_id: FileId) -> Option<Arc<Vec<u8>>> {
        self.inner.contents.get(&file_id).cloned()
    }
}

#[derive(Debug, Clone, Default)]
struct VfsInner {
    contents: HashMap<FileId, Arc<Vec<u8>>>,
    locations_to_ids: HashMap<PathBuf, FileId>,
    ids_to_locations: HashMap<FileId, Vec<PathBuf>>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde_with::SerializeDisplay,
    serde_with::DeserializeFromStr,
)]
pub struct FileId(blake3::Hash);

impl std::fmt::Display for FileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_hex())
    }
}

impl std::str::FromStr for FileId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hash = blake3::Hash::from_hex(s)?;
        Ok(Self(hash))
    }
}
