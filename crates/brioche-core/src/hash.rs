pub use crate::hash::blake3::Blake3Hash;

mod blake3;

#[derive(Debug, thiserror::Error)]
pub enum ParseHashError {
    #[error("expected string of length {expected}, was length {actual}")]
    WrongLength { expected: usize, actual: usize },
    #[error(transparent)]
    FromHexError(#[from] hex::FromHexError),
}
