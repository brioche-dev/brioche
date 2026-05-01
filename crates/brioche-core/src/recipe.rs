use std::collections::BTreeMap;

use bstr::BString;

use crate::{blob::BlobHash, encoding::TickEncoded};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RecipeHash(crate::hash::Blake3Hash);

impl std::fmt::Display for RecipeHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Artifact {
    #[serde(rename_all = "camelCase")]
    File(File),
    #[serde(rename_all = "camelCase")]
    Symlink {
        #[serde_as(as = "TickEncoded")]
        target: BString,
    },
    #[serde(rename_all = "camelCase")]
    Directory(Directory),
}

impl Artifact {
    pub fn hash(&self) -> RecipeHash {
        let mut hasher = blake3::Hasher::new();

        json_canon::to_writer(&mut hasher, self)
            .expect("Failed to serialize artifact while hashing");

        let hash = hasher.finalize();
        RecipeHash(hash.into())
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub content_blob: BlobHash,

    pub executable: bool,

    #[serde_as(as = "serde_with::TryFromInto<Artifact>")]
    pub resources: Directory,
}

#[serde_with::serde_as]
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    entries: BTreeMap<BString, RecipeHash>,
}

impl Directory {
    #[must_use]
    pub const fn from_entries(entries: BTreeMap<BString, RecipeHash>) -> Self {
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl From<Directory> for Artifact {
    fn from(value: Directory) -> Self {
        Self::Directory(value)
    }
}

impl TryFrom<Artifact> for Directory {
    type Error = DirectoryFromArtifactError;

    fn try_from(value: Artifact) -> Result<Self, Self::Error> {
        match value {
            Artifact::Directory(directory) => Ok(directory),
            Artifact::File(_) | Artifact::Symlink { .. } => {
                Err(DirectoryFromArtifactError::ExpectedDirectoryArtifact)
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum DirectoryFromArtifactError {
    #[error("expected directory artifact")]
    ExpectedDirectoryArtifact,
}
