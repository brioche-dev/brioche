use std::path::{Path, PathBuf};

use encoding::UrlEncoded;

pub mod autowrap;
mod encoding;
pub mod resources;

const MARKER: &[u8; 32] = b"brioche_pack_v0                 ";

const SEARCH_DEPTH_LIMIT: u32 = 64;

const LENGTH_BYTES: usize = 4;
type LengthInt = u32;

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pack {
    #[serde_as(as = "UrlEncoded")]
    pub program: bstr::BString,
    pub interpreter: Option<Interpreter>,
}

impl Pack {
    pub fn paths(&self) -> Vec<bstr::BString> {
        let Self {
            program,
            interpreter,
        } = self;

        let mut paths = vec![];

        paths.push(program.clone());
        if let Some(interpreter) = interpreter {
            match interpreter {
                Interpreter::LdLinux {
                    path,
                    library_paths,
                } => {
                    paths.push(path.clone());
                    paths.extend(library_paths.iter().cloned());
                }
            }
        }

        paths
    }
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum Interpreter {
    #[serde(rename_all = "camelCase")]
    LdLinux {
        #[serde_as(as = "UrlEncoded")]
        path: bstr::BString,
        #[serde_as(as = "Vec<UrlEncoded>")]
        library_paths: Vec<bstr::BString>,
    },
}

pub fn find_resource_dir(program: &Path) -> Result<PathBuf, PackResourceDirError> {
    if let Some(pack_resource_dir) = std::env::var_os("BRIOCHE_PACK_RESOURCES_DIR") {
        return Ok(PathBuf::from(pack_resource_dir));
    }

    let program = std::env::current_dir()?.join(program);

    let Some(mut current_dir) = program.parent() else {
        return Err(PackResourceDirError::NotFound);
    };

    for _ in 0..SEARCH_DEPTH_LIMIT {
        let pack_resource_dir = current_dir.join("brioche-pack.d");
        if pack_resource_dir.is_dir() {
            return Ok(pack_resource_dir);
        }

        let Some(parent) = current_dir.parent() else {
            return Err(PackResourceDirError::NotFound);
        };

        current_dir = parent;
    }

    Err(PackResourceDirError::DepthLimitReached)
}

pub fn inject_pack(mut writer: impl std::io::Write, pack: &Pack) -> Result<(), InjectPackError> {
    let pack_bytes = serde_json::to_vec(pack).map_err(InjectPackError::SerializeError)?;
    let pack_length: LengthInt = pack_bytes
        .len()
        .try_into()
        .map_err(|_| InjectPackError::PackTooLarge)?;
    let length_bytes = pack_length.to_le_bytes();

    writer.write_all(MARKER)?;
    writer.write_all(&length_bytes)?;
    writer.write_all(&pack_bytes)?;
    writer.write_all(&length_bytes)?;
    writer.write_all(MARKER)?;

    Ok(())
}

pub fn extract_pack(mut reader: impl std::io::Read) -> Result<Pack, ExtractPackError> {
    let mut program = vec![];
    reader
        .read_to_end(&mut program)
        .map_err(ExtractPackError::ReadPackedProgramError)?;

    let program = program
        .strip_suffix(MARKER)
        .ok_or_else(|| ExtractPackError::MarkerNotFound)?;
    let (program, length_bytes) = program.split_at(program.len().wrapping_sub(LENGTH_BYTES));
    let length_bytes: [u8; LENGTH_BYTES] = length_bytes
        .try_into()
        .map_err(|_| ExtractPackError::MalformedMarker)?;
    let length = LengthInt::from_le_bytes(length_bytes);
    let length: usize = length
        .try_into()
        .map_err(|_| ExtractPackError::MalformedMarker)?;

    let (program, pack) = program.split_at(program.len().wrapping_sub(length));
    let program = program
        .strip_suffix(&length_bytes)
        .ok_or_else(|| ExtractPackError::MalformedMarker)?;
    let _program = program
        .strip_suffix(MARKER)
        .ok_or_else(|| ExtractPackError::MalformedMarker)?;

    let pack = serde_json::from_slice(pack).map_err(ExtractPackError::InvalidPack)?;

    Ok(pack)
}

#[derive(Debug, thiserror::Error)]
pub enum PackResourceDirError {
    #[error("brioche pack resource dir not found")]
    NotFound,
    #[error("error while searching for brioche pack resource dir: {0}")]
    IoError(#[from] std::io::Error),
    #[error("reached depth limit while searching for brioche pack resource dir")]
    DepthLimitReached,
}

#[derive(Debug, thiserror::Error)]
pub enum InjectPackError {
    #[error("failed to write packed program: {0}")]
    IoError(#[from] std::io::Error),
    #[error("failed to serialize pack: {0}")]
    SerializeError(#[source] serde_json::Error),
    #[error("pack JSON too large")]
    PackTooLarge,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractPackError {
    #[error("failed to read packed program: {0}")]
    ReadPackedProgramError(#[source] std::io::Error),
    #[error("marker not found at end of the packed program")]
    MarkerNotFound,
    #[error("marker was malformed at the end of the packed program")]
    MalformedMarker,
    #[error("failed to parse pack: {0}")]
    InvalidPack(#[source] serde_json::Error),
}
