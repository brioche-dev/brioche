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

#[derive(Debug, thiserror::Error)]
pub enum ParseHashError {
    #[error("expected string of length {expected}, was length {actual}")]
    WrongLength { expected: usize, actual: usize },
    #[error(transparent)]
    FromHexError(#[from] hex::FromHexError),
}
