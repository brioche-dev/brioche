use crate::hash::sha256::Sha256Hasher;
pub use crate::hash::{blake3::Blake3Hash, sha256::Sha256Hash};

mod blake3;
mod sha256;

#[serde_with::serde_as]
#[derive(Debug, Clone, Hash, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum AnyHash {
    Sha256 { value: Sha256Hash },
}

impl std::fmt::Display for AnyHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sha256 { value } => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseHashError {
    #[error("expected string of length {expected}, was length {actual}")]
    WrongLength { expected: usize, actual: usize },
    #[error(transparent)]
    FromHexError(#[from] hex::FromHexError),
}

pub enum AnyHashHasher {
    Sha256(Sha256Hasher),
}

impl AnyHashHasher {
    #[must_use]
    pub fn new_sha256() -> Self {
        Self::Sha256(Sha256Hasher::new())
    }

    #[must_use]
    pub fn for_hash(hash: &AnyHash) -> Self {
        match hash {
            AnyHash::Sha256 { .. } => Self::Sha256(Sha256Hasher::new()),
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(bytes),
        }
    }

    pub fn finish(self) -> AnyHash {
        match self {
            Self::Sha256(hasher) => {
                let hash = hasher.finish();
                AnyHash::Sha256 { value: hash }
            }
        }
    }
}
