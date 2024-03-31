use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use anyhow::Context as _;

#[derive(Debug, Clone, Default)]
pub struct Vfs {
    inner: Arc<RwLock<VfsInner>>,
}

impl Vfs {
    pub async fn load(&self, path: &Path) -> anyhow::Result<(FileId, Arc<Vec<u8>>)> {
        let path = crate::fs_utils::logical_path(path);

        {
            if let Some((file_id, contents)) = self.load_cached(&path)? {
                return Ok((file_id, contents));
            }
        }

        let contents = tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read file {}", path.display()))?;
        let contents = Arc::new(contents);

        let hash = blake3::hash(&contents);
        let file_id = FileId(hash);

        let mut vfs = self
            .inner
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire VFS lock"))?;
        vfs.contents.insert(file_id, contents.clone());
        vfs.locations_to_ids.insert(path.clone(), file_id);
        vfs.ids_to_locations
            .entry(file_id)
            .or_default()
            .push(path.clone());

        tracing::debug!(path = %path.display(), %file_id, "loaded file into VFS");

        Ok((file_id, contents))
    }

    pub fn load_cached(&self, path: &Path) -> anyhow::Result<Option<(FileId, Arc<Vec<u8>>)>> {
        let path = crate::fs_utils::logical_path(path);

        let vfs = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire VFS lock"))?;
        let Some(file_id) = vfs.locations_to_ids.get(&path) else {
            return Ok(None);
        };

        let contents = vfs.contents[file_id].clone();
        Ok(Some((*file_id, contents)))
    }

    pub fn read(&self, file_id: FileId) -> anyhow::Result<Option<Arc<Vec<u8>>>> {
        let vfs = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire VFS lock"))?;
        Ok(vfs.contents.get(&file_id).cloned())
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
