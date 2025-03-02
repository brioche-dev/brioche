use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use anyhow::Context as _;

use crate::{Brioche, blob::BlobHash};

#[derive(Debug, Clone)]
pub struct Vfs {
    mutable: bool,
    inner: Arc<RwLock<VfsInner>>,
}

impl Vfs {
    pub fn immutable() -> Self {
        Self {
            mutable: false,
            inner: Default::default(),
        }
    }

    pub fn mutable() -> Self {
        Self {
            mutable: true,
            inner: Default::default(),
        }
    }

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

        let file_id = if self.mutable {
            FileId::Mutable(ulid::Ulid::new())
        } else {
            FileId::Hash(BlobHash::for_content(&contents))
        };

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

    pub fn update(&self, file_id: FileId, contents: Arc<Vec<u8>>) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(file_id, FileId::Mutable(_)),
            "file must be mutable"
        );

        let mut vfs = self
            .inner
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire VFS lock"))?;
        vfs.contents.insert(file_id, contents);

        let locations = vfs.ids_to_locations.get_mut(&file_id).unwrap();
        for location in locations.iter() {
            tracing::debug!(path = %location.display(), %file_id, "edited file in VFS");
        }

        Ok(())
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

pub async fn commit_blob(brioche: &Brioche, file_id: FileId) -> anyhow::Result<BlobHash> {
    let expected_blob_hash = match file_id {
        FileId::Hash(hash) => Some(hash),
        FileId::Mutable(_) => None,
    };
    let content = brioche.vfs.read(file_id)?;
    let Some(content) = content else {
        return Err(anyhow::anyhow!("file not loaded: {file_id}"));
    };

    let mut permit = crate::blob::get_save_blob_permit().await?;
    let blob_hash = crate::blob::save_blob(
        brioche,
        &mut permit,
        &content[..],
        crate::blob::SaveBlobOptions::default().expected_blob_hash(expected_blob_hash),
    )
    .await?;

    Ok(blob_hash)
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
pub enum FileId {
    Hash(BlobHash),
    Mutable(ulid::Ulid),
}

impl FileId {
    pub fn as_blob_hash(&self) -> anyhow::Result<BlobHash> {
        match self {
            Self::Hash(hash) => Ok(*hash),
            Self::Mutable(_) => anyhow::bail!("tried to get blob hash for mutable file"),
        }
    }
}

impl std::fmt::Display for FileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hash(hash) => write!(f, "{hash}"),
            Self::Mutable(ulid) => write!(f, "{ulid}"),
        }
    }
}

impl std::str::FromStr for FileId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.len() {
            26 => {
                let ulid = ulid::Ulid::from_str(s)?;
                Ok(Self::Mutable(ulid))
            }
            64 => {
                let hash = s.parse()?;
                Ok(Self::Hash(hash))
            }
            _ => anyhow::bail!("invalid file ID: {}", s),
        }
    }
}
