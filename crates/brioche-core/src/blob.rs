use std::{
    borrow::Cow,
    io::{Read as _, Write as _},
    os::unix::prelude::PermissionsExt as _,
    path::{Path, PathBuf},
};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{
    BriocheResources,
    hash::{AnyHash, AnyHashHasher},
};

pub struct SaveBlobPermit<'a> {
    _permit: tokio::sync::SemaphorePermit<'a>,
}

pub const MAX_CONCURRENT_BLOB_SAVES: usize = 10;

static SAVE_BLOB_SEMAPHORE: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_BLOB_SAVES);

pub async fn get_save_blob_permit<'a>() -> SaveBlobPermit<'a> {
    let permit = SAVE_BLOB_SEMAPHORE
        .acquire()
        .await
        .expect("failed to acquire save blob permit");
    SaveBlobPermit { _permit: permit }
}

pub async fn save_blob(
    brioche: &BriocheResources,
    _permit: &mut SaveBlobPermit<'_>,
    bytes: &[u8],
    options: SaveBlobOptions<'_>,
) -> Result<BlobHash, SaveBlobError> {
    let mut hasher = BlobHasher::new(&options);
    hasher.update(bytes);
    let (blob_hash, _validated_hash) = hasher.finish()?;

    let blob_path = local_blob_path(brioche, blob_hash);

    if let Some(parent) = blob_path.parent() {
        tokio::fs::create_dir_all(&parent)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("failed to create dir '{}'", parent.display()).into(),
            })?;
    }

    let already_exists =
        tokio::fs::try_exists(&blob_path)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("failed to check path '{}'", blob_path.display()).into(),
            })?;
    if already_exists {
        return Ok(blob_hash);
    }

    let temp_dir = brioche.data_dir.join("blobs-temp");
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .map_err(|error| SaveBlobError::IoError {
            error,
            message: format!("failed to create dir '{}'", temp_dir.display()).into(),
        })?;
    let temp_path = temp_dir.join(ulid::Ulid::new().to_string());

    let mut temp_file =
        tokio::fs::File::create(&temp_path)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("failed to create temp blob '{}'", temp_path.display()).into(),
            })?;
    temp_file
        .write_all(bytes)
        .await
        .map_err(|error| SaveBlobError::IoError {
            error,
            message: format!("failed to write temp blob '{}'", temp_path.display()).into(),
        })?;
    let temp_file = temp_file.into_std().await;
    tokio::task::spawn_blocking(move || {
        temp_file.set_permissions(blob_permissions())?;
        temp_file.set_modified(crate::fs_utils::brioche_epoch())?;
        std::io::Result::Ok(())
    })
    .await
    .unwrap()
    .map_err(|error| SaveBlobError::IoError {
        error,
        message: format!("failed to set blob metadata '{}'", temp_path.display()).into(),
    })?;

    tokio::fs::rename(&temp_path, &blob_path)
        .await
        .map_err(|error| SaveBlobError::IoError {
            error,
            message: format!(
                "failed to move temp blob '{}' to final path '{}'",
                temp_path.display(),
                blob_path.display()
            )
            .into(),
        })?;

    Ok(blob_hash)
}

pub async fn save_blob_from_reader<R>(
    brioche: &BriocheResources,
    _permit: &mut SaveBlobPermit<'_>,
    mut input: R,
    mut options: SaveBlobOptions<'_>,
    buffer: &mut Vec<u8>,
) -> Result<BlobHash, SaveBlobError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    assert!(
        !options.remove_input,
        "called save_blob_from_reader with remove_input set"
    );

    let mut hasher = BlobHasher::new(&options);

    let temp_dir = brioche.data_dir.join("blobs-temp");
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .map_err(|error| SaveBlobError::IoError {
            error,
            message: format!("failed to create dir '{}'", temp_dir.display()).into(),
        })?;
    let temp_path = temp_dir.join(ulid::Ulid::new().to_string());
    let mut temp_file =
        tokio::fs::File::create(&temp_path)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("failed to create temp blob '{}'", temp_path.display()).into(),
            })?;

    tracing::trace!(temp_path = %temp_path.display(), "saving blob");

    buffer.resize(1024 * 1024, 0);
    let mut total_bytes_read = 0;
    loop {
        let length = input
            .read(buffer)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: "error while reading blob".into(),
            })?;
        if length == 0 {
            break;
        }

        total_bytes_read += length;
        let buffer = &buffer[..length];

        temp_file
            .write_all(buffer)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("error while writing temp blob '{}'", temp_path.display()).into(),
            })?;

        hasher.update(buffer);

        if let Some(on_progress) = &mut options.on_progress {
            on_progress(total_bytes_read);
        }
    }

    let (blob_hash, _validated_hash) = hasher.finish()?;
    let blob_path = local_blob_path(brioche, blob_hash);

    if let Some(parent) = blob_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("failed to create dir '{}'", parent.display()).into(),
            })?;
    }

    tracing::debug!(overwrite = blob_path.exists(), %blob_hash, "saved blob");

    let temp_file = temp_file.into_std().await;
    tokio::task::spawn_blocking(move || {
        temp_file.set_permissions(blob_permissions())?;
        temp_file.set_modified(crate::fs_utils::brioche_epoch())?;
        std::io::Result::Ok(())
    })
    .await
    .unwrap()
    .map_err(|error| SaveBlobError::IoError {
        error,
        message: format!("failed to set blob metadata '{}'", temp_path.display()).into(),
    })?;

    tokio::fs::rename(&temp_path, &blob_path)
        .await
        .map_err(|error| SaveBlobError::IoError {
            error,
            message: format!(
                "failed to move temp blob '{}' to final path '{}'",
                temp_path.display(),
                blob_path.display()
            )
            .into(),
        })?;

    Ok(blob_hash)
}

pub fn save_blob_from_reader_sync<R>(
    brioche: &BriocheResources,
    _permit: &mut SaveBlobPermit<'_>,
    mut input: R,
    mut options: SaveBlobOptions<'_>,
    buffer: &mut Vec<u8>,
) -> Result<BlobHash, SaveBlobError>
where
    R: std::io::Read,
{
    assert!(
        !options.remove_input,
        "called save_blob_from_reader_sync with remove_input set"
    );
    assert!(
        options.expected_hash.is_none(),
        "called save_blob_from_reader with expected_hash, but cannot validate hash in sync mode"
    );

    let mut hasher = BlobHasher::new(&options);

    let temp_dir = brioche.data_dir.join("blobs-temp");
    std::fs::create_dir_all(&temp_dir).map_err(|error| SaveBlobError::IoError {
        error,
        message: format!("failed to create dir '{}'", temp_dir.display()).into(),
    })?;
    let temp_path = temp_dir.join(ulid::Ulid::new().to_string());
    let mut temp_file =
        std::fs::File::create(&temp_path).map_err(|error| SaveBlobError::IoError {
            error,
            message: format!("failed to create temp blob '{}'", temp_path.display()).into(),
        })?;

    tracing::trace!(temp_path = %temp_path.display(), "saving blob");

    buffer.resize(1024 * 1024, 0);
    let mut total_bytes_read = 0;
    loop {
        let length = input.read(buffer).map_err(|error| SaveBlobError::IoError {
            error,
            message: "error while reading blob".into(),
        })?;
        if length == 0 {
            break;
        }

        total_bytes_read += length;
        let buffer = &buffer[..length];

        temp_file
            .write_all(buffer)
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("error while writing temp blob '{}'", temp_path.display()).into(),
            })?;

        hasher.update(buffer);

        if let Some(on_progress) = &mut options.on_progress {
            on_progress(total_bytes_read);
        }
    }

    let (blob_hash, _validated_hash) = hasher.finish()?;
    let blob_path = local_blob_path(brioche, blob_hash);

    if let Some(parent) = blob_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| SaveBlobError::IoError {
            error,
            message: format!("failed to create dir '{}'", parent.display()).into(),
        })?;
    }

    tracing::debug!(overwrite = blob_path.exists(), %blob_hash, "saved blob");

    temp_file
        .set_permissions(blob_permissions())
        .map_err(|error| SaveBlobError::IoError {
            error,
            message: format!("failed to set blob permissions '{}'", temp_path.display()).into(),
        })?;
    temp_file
        .set_modified(crate::fs_utils::brioche_epoch())
        .map_err(|error| SaveBlobError::IoError {
            error,
            message: format!("failed to set blob modified time '{}'", temp_path.display()).into(),
        })?;

    std::fs::rename(&temp_path, &blob_path).map_err(|error| SaveBlobError::IoError {
        error,
        message: format!(
            "failed to move temp blob '{}' to final path '{}'",
            temp_path.display(),
            blob_path.display()
        )
        .into(),
    })?;

    Ok(blob_hash)
}

pub async fn save_blob_from_file(
    brioche: &BriocheResources,
    _permit: &mut SaveBlobPermit<'_>,
    input_path: &Path,
    options: SaveBlobOptions<'_>,
    buffer: &mut Vec<u8>,
) -> Result<BlobHash, SaveBlobError> {
    let mut hasher = BlobHasher::new(&options);

    let (mut swapped_buffer, hasher) = tokio::task::spawn_blocking({
        let mut buffer = std::mem::take(buffer);
        let input_path = input_path.to_owned();
        move || {
            buffer.resize(1024 * 1024, 0);
            let mut input_file =
                std::fs::File::open(&input_path).map_err(|error| SaveBlobError::IoError {
                    error,
                    message: format!("failed to open input file '{}'", input_path.display()).into(),
                })?;
            loop {
                let length =
                    input_file
                        .read(&mut buffer)
                        .map_err(|error| SaveBlobError::IoError {
                            error,
                            message: "error while reading input file".into(),
                        })?;
                if length == 0 {
                    break;
                }

                let buffer = &buffer[..length];

                hasher.update(buffer);
            }

            Ok::<_, SaveBlobError>((buffer, hasher))
        }
    })
    .await
    .unwrap()?;

    std::mem::swap(buffer, &mut swapped_buffer);

    let (blob_hash, _validated_hash) = hasher.finish()?;
    let blob_path = local_blob_path(brioche, blob_hash);

    if let Some(parent) = blob_path.parent() {
        tokio::fs::create_dir_all(&parent)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("failed to create dir '{}'", parent.display()).into(),
            })?;
    }

    let existing_blob_file = match tokio::fs::File::open(&blob_path).await {
        Ok(file) => Some(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(SaveBlobError::IoError {
                error,
                message: format!("failed to open blob file '{}'", blob_path.display()).into(),
            });
        }
    };

    let input_metadata =
        tokio::fs::metadata(&input_path)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!(
                    "failed to get metadata for input file '{}'",
                    input_path.display()
                )
                .into(),
            })?;

    let permissions = blob_permissions();
    if let Some(existing_blob_file) = existing_blob_file {
        // The blob file already exists, so don't try to create it again. But
        // we may still need to remove the input file
        if options.remove_input {
            tokio::fs::remove_file(input_path)
                .await
                .map_err(|error| SaveBlobError::IoError {
                    error,
                    message: format!("failed to remove input file '{}'", input_path.display())
                        .into(),
                })?;
        }

        // Make sure the blob's permissions and modified time are set properly
        let existing_blob_file = existing_blob_file.into_std().await;
        tokio::task::spawn_blocking(move || {
            existing_blob_file.set_permissions(blob_permissions())?;
            existing_blob_file.set_modified(crate::fs_utils::brioche_epoch())?;
            std::io::Result::Ok(())
        })
        .await
        .unwrap()
        .map_err(|error| SaveBlobError::IoError {
            error,
            message: format!("failed to set blob metadata '{}'", blob_path.display()).into(),
        })?;
    } else if options.remove_input && is_file_exclusive(&input_metadata) {
        // Since this file is exclusive (i.e. has no hardlinks), we can
        // change its permissions and move it into place. We need to check
        // for exclusivity, because we would otherwise ruin the permission
        // of other hard links to the same file.

        tokio::fs::set_permissions(input_path, permissions)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("failed to set blob permissions '{}'", input_path.display())
                    .into(),
            })?;
        crate::fs_utils::set_mtime_to_brioche_epoch(input_path)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!(
                    "failed to set blob modified time '{}'",
                    input_path.display()
                )
                .into(),
            })?;
        let move_type = crate::fs_utils::move_file(input_path, &blob_path)
            .await
            .map_err(|error| SaveBlobError::AtomicIoError {
                error,
                message: "failed to move file while saving blob".into(),
            })?;
        tracing::debug!(input_path = %input_path.display(), %blob_hash, ?move_type, "saved blob by moving file");
    } else {
        crate::fs_utils::atomic_copy(input_path, &blob_path)
            .await
            .map_err(|error| SaveBlobError::AtomicIoError {
                error,
                message: "failed to copy file while saving blob".into(),
            })?;
        tokio::fs::set_permissions(&blob_path, permissions)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!("failed to set blob permissions '{}'", input_path.display())
                    .into(),
            })?;
        crate::fs_utils::set_mtime_to_brioche_epoch(input_path)
            .await
            .map_err(|error| SaveBlobError::IoError {
                error,
                message: format!(
                    "failed to set blob modified time '{}'",
                    input_path.display()
                )
                .into(),
            })?;
        tracing::debug!(input_path = %input_path.display(), %blob_hash, "saved blob by copying file");

        if options.remove_input {
            tokio::fs::remove_file(input_path)
                .await
                .map_err(|error| SaveBlobError::IoError {
                    error,
                    message: format!("failed to remove input file '{}'", input_path.display())
                        .into(),
                })?;
        }
    }

    Ok(blob_hash)
}

#[derive(Default)]
pub struct SaveBlobOptions<'a> {
    expected_hash: Option<AnyHash>,
    expected_blob_hash: Option<BlobHash>,
    on_progress: Option<Box<dyn FnMut(usize) + Send + 'a>>,
    remove_input: bool,
}

impl<'a> SaveBlobOptions<'a> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn expected_hash(mut self, expected_hash: Option<AnyHash>) -> Self {
        self.expected_hash = expected_hash;
        self
    }

    #[must_use]
    pub const fn expected_blob_hash(mut self, expected_blob_hash: Option<BlobHash>) -> Self {
        self.expected_blob_hash = expected_blob_hash;
        self
    }

    #[must_use]
    pub fn on_progress(mut self, on_progress: impl FnMut(usize) + Send + 'a) -> Self {
        self.on_progress = Some(Box::new(on_progress));
        self
    }

    #[must_use]
    pub const fn remove_input(mut self, remove_input: bool) -> Self {
        self.remove_input = remove_input;
        self
    }
}

pub async fn blob_path(
    brioche: &BriocheResources,
    blob_hash: BlobHash,
) -> Result<PathBuf, GetBlobError> {
    let local_path = local_blob_path(brioche, blob_hash);

    let blob_exists =
        tokio::fs::try_exists(&local_path)
            .await
            .map_err(|error| GetBlobError::IoError {
                error,
                message: format!("failed to access blob '{}'", local_path.display()).into(),
            })?;
    if blob_exists {
        Ok(local_path)
    } else {
        Err(GetBlobError::BlobNotFoundLocally(blob_hash))
    }
}

#[must_use]
pub fn local_blob_path(brioche: &BriocheResources, blob_hash: BlobHash) -> PathBuf {
    let blobs_dir = brioche.data_dir.join("blobs");
    blobs_dir.join(hex::encode(blob_hash.0.as_bytes()))
}

fn blob_permissions() -> std::fs::Permissions {
    std::fs::Permissions::from_mode(0o444)
}

fn is_file_exclusive(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    metadata.nlink() == 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct BlobHash(crate::hash::Blake3Hash);

impl BlobHash {
    #[must_use]
    pub const fn from_blake3(hash: crate::hash::Blake3Hash) -> Self {
        Self(hash)
    }

    #[must_use]
    pub const fn to_blake3(self) -> crate::hash::Blake3Hash {
        self.0
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl Ord for BlobHash {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_bytes().cmp(other.0.as_bytes())
    }
}

impl PartialOrd for BlobHash {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for BlobHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

struct BlobHasher {
    hasher: blake3::Hasher,
    expected_blob_hash: Option<BlobHash>,
    validation_hash_with_hasher: Option<(AnyHash, AnyHashHasher)>,
}

impl BlobHasher {
    fn new(options: &SaveBlobOptions<'_>) -> Self {
        let hasher = blake3::Hasher::new();
        let validation_hash_with_hasher = options
            .expected_hash
            .as_ref()
            .map(|hash| (hash.clone(), AnyHashHasher::for_hash(hash)));

        Self {
            hasher,
            expected_blob_hash: options.expected_blob_hash,
            validation_hash_with_hasher,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);

        if let Some((_, validation_hasher)) = &mut self.validation_hash_with_hasher {
            validation_hasher.update(bytes);
        }
    }

    fn finish(self) -> Result<(BlobHash, Option<AnyHash>), BlobHashMismatchError> {
        let validated_hash =
            if let Some((expected_hash, validation_hasher)) = self.validation_hash_with_hasher {
                let actual_hash = validation_hasher.finish();

                if expected_hash != actual_hash {
                    return Err(BlobHashMismatchError::AnyHashMismatch {
                        expected: expected_hash,
                        actual: actual_hash,
                    });
                }

                Some(actual_hash)
            } else {
                None
            };

        let hash = self.hasher.finalize();
        let blob_hash = BlobHash(hash.into());

        if let Some(expected_blob_hash) = self.expected_blob_hash
            && blob_hash != expected_blob_hash
        {
            return Err(BlobHashMismatchError::BlobHashMismatch {
                expected: expected_blob_hash,
                actual: blob_hash,
            });
        }

        Ok((blob_hash, validated_hash))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlobHashMismatchError {
    #[error("expected hash {expected} but got {actual}")]
    AnyHashMismatch { expected: AnyHash, actual: AnyHash },

    #[error("expected hash {expected} but got {actual}")]
    BlobHashMismatch {
        expected: BlobHash,
        actual: BlobHash,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum GetBlobError {
    #[error("{message}: {error}")]
    IoError {
        #[source]
        error: std::io::Error,

        message: Cow<'static, str>,
    },

    #[error("could not find blob locally: {0}")]
    BlobNotFoundLocally(BlobHash),
}

#[derive(Debug, thiserror::Error)]
pub enum SaveBlobError {
    #[error(transparent)]
    BlobHashMismatch(#[from] BlobHashMismatchError),

    #[error("{message}: {error}")]
    IoError {
        #[source]
        error: std::io::Error,

        message: Cow<'static, str>,
    },

    #[error("{message}: {error}")]
    AtomicIoError {
        #[source]
        error: crate::fs_utils::AtomicIoError,

        message: Cow<'static, str>,
    },
}
