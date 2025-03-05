use std::{
    io::{Read as _, Write as _},
    os::unix::prelude::PermissionsExt as _,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use sqlx::Acquire as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{Brioche, Hash};

pub struct SaveBlobPermit<'a> {
    _permit: tokio::sync::SemaphorePermit<'a>,
}

pub const MAX_CONCURRENT_BLOB_SAVES: usize = 10;

static SAVE_BLOB_SEMAPHORE: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_BLOB_SAVES);

pub async fn get_save_blob_permit<'a>() -> anyhow::Result<SaveBlobPermit<'a>> {
    let permit = SAVE_BLOB_SEMAPHORE
        .acquire()
        .await
        .context("failed to acquire permit to save blob")?;
    Ok(SaveBlobPermit { _permit: permit })
}

pub async fn save_blob(
    brioche: &Brioche,
    _permit: &mut SaveBlobPermit<'_>,
    bytes: &[u8],
    options: SaveBlobOptions<'_>,
) -> anyhow::Result<BlobHash> {
    let mut hasher = BlobHasher::new(&options);
    hasher.update(bytes);
    let (blob_hash, validated_hash) = hasher.finish()?;

    let blob_path = local_blob_path(brioche, blob_hash);

    if let Some(validated_hash) = validated_hash {
        let validated_hash_string = validated_hash.to_string();
        let blob_hash_string = blob_hash.to_string();

        let mut db_conn = brioche.db_conn.lock().await;
        let mut db_transaction = db_conn.begin().await?;
        sqlx::query!(
            r"
                INSERT INTO blob_aliases (hash, blob_hash) VALUES (?, ?)
                ON CONFLICT (hash) DO UPDATE SET blob_hash = ?
            ",
            validated_hash_string,
            blob_hash_string,
            blob_hash_string,
        )
        .execute(&mut *db_transaction)
        .await?;
        db_transaction.commit().await?;
        drop(db_conn);
    }

    if let Some(parent) = blob_path.parent() {
        tokio::fs::create_dir_all(&parent)
            .await
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    if tokio::fs::try_exists(&blob_path).await? {
        return Ok(blob_hash);
    }

    let temp_dir = brioche.data_dir.join("blobs-temp");
    tokio::fs::create_dir_all(&temp_dir).await.unwrap();
    let temp_path = temp_dir.join(ulid::Ulid::new().to_string());

    let mut temp_file = tokio::fs::File::create(&temp_path)
        .await
        .context("failed to open temp file")?;
    temp_file
        .write_all(bytes)
        .await
        .context("failed to write blob to temp file")?;
    temp_file
        .set_permissions(blob_permissions())
        .await
        .context("failed to set blob permissions")?;
    let temp_file = temp_file.into_std().await;
    tokio::task::spawn_blocking(move || {
        temp_file.set_modified(crate::fs_utils::brioche_epoch())?;
        anyhow::Ok(())
    })
    .await??;

    tokio::fs::rename(&temp_path, &blob_path)
        .await
        .context("failed to rename blob from temp file")?;

    Ok(blob_hash)
}

pub async fn save_blob_from_reader<R>(
    brioche: &Brioche,
    _permit: &mut SaveBlobPermit<'_>,
    mut input: R,
    mut options: SaveBlobOptions<'_>,
    buffer: &mut Vec<u8>,
) -> anyhow::Result<BlobHash>
where
    R: tokio::io::AsyncRead + Unpin,
{
    anyhow::ensure!(!options.remove_input, "cannot remove input from reader");

    let mut hasher = BlobHasher::new(&options);

    let temp_dir = brioche.data_dir.join("blobs-temp");
    tokio::fs::create_dir_all(&temp_dir).await.unwrap();
    let temp_path = temp_dir.join(ulid::Ulid::new().to_string());
    let mut temp_file = tokio::fs::File::create(&temp_path)
        .await
        .context("failed to open temp file")?;

    tracing::trace!(temp_path = %temp_path.display(), "saving blob");

    buffer.resize(1024 * 1024, 0);
    let mut total_bytes_read = 0;
    loop {
        let length = input.read(buffer).await.context("failed to read")?;
        if length == 0 {
            break;
        }

        total_bytes_read += length;
        let buffer = &buffer[..length];

        temp_file
            .write_all(buffer)
            .await
            .context("failed to write all")?;

        hasher.update(buffer);

        if let Some(on_progress) = &mut options.on_progress {
            on_progress(total_bytes_read)?;
        }
    }

    let (blob_hash, validated_hash) = hasher.finish()?;
    let blob_path = local_blob_path(brioche, blob_hash);

    if let Some(validated_hash) = validated_hash {
        let validated_hash_string = validated_hash.to_string();
        let blob_hash_string = blob_hash.to_string();

        let mut db_conn = brioche.db_conn.lock().await;
        let mut db_transaction = db_conn.begin().await?;
        sqlx::query!(
            r"
                INSERT INTO blob_aliases (hash, blob_hash) VALUES (?, ?)
                ON CONFLICT (hash) DO UPDATE SET blob_hash = ?
            ",
            validated_hash_string,
            blob_hash_string,
            blob_hash_string,
        )
        .execute(&mut *db_transaction)
        .await?;
        db_transaction.commit().await?;
        drop(db_conn);
    }

    if let Some(parent) = blob_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tracing::debug!(overwrite = blob_path.exists(), %blob_hash, "saved blob");

    temp_file
        .set_permissions(blob_permissions())
        .await
        .context("failed to set blob permissions")?;
    let temp_file = temp_file.into_std().await;
    tokio::task::spawn_blocking(move || {
        temp_file.set_modified(crate::fs_utils::brioche_epoch())?;
        anyhow::Ok(())
    })
    .await??;

    tokio::fs::rename(&temp_path, &blob_path)
        .await
        .context("failed to rename blob from temp file")?;

    Ok(blob_hash)
}

pub fn save_blob_from_reader_sync<R>(
    brioche: &Brioche,
    _permit: &mut SaveBlobPermit<'_>,
    mut input: R,
    mut options: SaveBlobOptions<'_>,
    buffer: &mut Vec<u8>,
) -> anyhow::Result<BlobHash>
where
    R: std::io::Read,
{
    anyhow::ensure!(!options.remove_input, "cannot remove input from reader");
    anyhow::ensure!(
        options.expected_hash.is_none(),
        "cannot validate expected hash in sync mode"
    );

    let mut hasher = BlobHasher::new(&options);

    let temp_dir = brioche.data_dir.join("blobs-temp");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let temp_path = temp_dir.join(ulid::Ulid::new().to_string());
    let mut temp_file = std::fs::File::create(&temp_path).context("failed to open temp file")?;

    tracing::trace!(temp_path = %temp_path.display(), "saving blob");

    buffer.resize(1024 * 1024, 0);
    let mut total_bytes_read = 0;
    loop {
        let length = input.read(buffer).context("failed to read")?;
        if length == 0 {
            break;
        }

        total_bytes_read += length;
        let buffer = &buffer[..length];

        temp_file.write_all(buffer).context("failed to write all")?;

        hasher.update(buffer);

        if let Some(on_progress) = &mut options.on_progress {
            on_progress(total_bytes_read)?;
        }
    }

    let (blob_hash, _validated_hash) = hasher.finish()?;
    let blob_path = local_blob_path(brioche, blob_hash);

    if let Some(parent) = blob_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    tracing::debug!(overwrite = blob_path.exists(), %blob_hash, "saved blob");

    temp_file
        .set_permissions(blob_permissions())
        .context("failed to set blob permissions")?;
    temp_file.set_modified(crate::fs_utils::brioche_epoch())?;

    std::fs::rename(&temp_path, &blob_path).context("failed to rename blob from temp file")?;

    Ok(blob_hash)
}

pub async fn save_blob_from_file(
    brioche: &Brioche,
    _permit: &mut SaveBlobPermit<'_>,
    input_path: &Path,
    options: SaveBlobOptions<'_>,
    buffer: &mut Vec<u8>,
) -> anyhow::Result<BlobHash> {
    let mut hasher = BlobHasher::new(&options);

    let (mut swapped_buffer, hasher) = tokio::task::spawn_blocking({
        let mut buffer = std::mem::take(buffer);
        let input_path = input_path.to_owned();
        move || {
            buffer.resize(1024 * 1024, 0);
            let mut input_file = std::fs::File::open(&input_path)
                .with_context(|| format!("failed to open input file {}", input_path.display()))?;
            loop {
                let length = input_file.read(&mut buffer).context("failed to read")?;
                if length == 0 {
                    break;
                }

                let buffer = &buffer[..length];

                hasher.update(buffer);
            }

            anyhow::Ok((buffer, hasher))
        }
    })
    .await??;

    std::mem::swap(buffer, &mut swapped_buffer);

    let (blob_hash, validated_hash) = hasher.finish()?;
    let blob_path = local_blob_path(brioche, blob_hash);

    if let Some(validated_hash) = validated_hash {
        let validated_hash_string = validated_hash.to_string();
        let blob_hash_string = blob_hash.to_string();

        let mut db_conn = brioche.db_conn.lock().await;
        let mut db_transaction = db_conn.begin().await?;
        sqlx::query!(
            r"
                INSERT INTO blob_aliases (hash, blob_hash) VALUES (?, ?)
                ON CONFLICT (hash) DO UPDATE SET blob_hash = ?
            ",
            validated_hash_string,
            blob_hash_string,
            blob_hash_string,
        )
        .execute(&mut *db_transaction)
        .await?;
        db_transaction.commit().await?;
        drop(db_conn);
    }

    if let Some(parent) = blob_path.parent() {
        tokio::fs::create_dir_all(&parent)
            .await
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let existing_blob_file = match tokio::fs::File::open(&blob_path).await {
        Ok(file) => Some(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to open blob file at {}", blob_path.display()));
        }
    };

    let input_metadata = tokio::fs::metadata(&input_path).await.with_context(|| {
        format!(
            "failed to get metadata for input file {}",
            input_path.display()
        )
    })?;

    let permissions = blob_permissions();
    if let Some(existing_blob_file) = existing_blob_file {
        // The blob file already exists, so don't try to create it again. But
        // we may still need to remove the input file
        if options.remove_input {
            tokio::fs::remove_file(input_path)
                .await
                .with_context(|| format!("failed to remove input file {}", input_path.display()))?;
        }

        // Make sure the blob's permissions and modified time are set properly
        existing_blob_file
            .set_permissions(permissions)
            .await
            .context("failed to set blob permissions")?;
        let existing_blob_file = existing_blob_file.into_std().await;
        tokio::task::spawn_blocking(move || {
            existing_blob_file.set_modified(crate::fs_utils::brioche_epoch())?;
            anyhow::Ok(())
        })
        .await??;
    } else if options.remove_input && is_file_exclusive(&input_metadata) {
        // Since this file is exclusive (i.e. has no hardlinks), we can
        // change its permissions and move it into place. We need to check
        // for exclusivity, because we would otherwise ruin the permission
        // of other hard links to the same file.

        tokio::fs::set_permissions(input_path, permissions)
            .await
            .context("failed to set blob permissions")?;
        crate::fs_utils::set_mtime_to_brioche_epoch(input_path)
            .await
            .context("failed to set blob modified time")?;
        let move_type = crate::fs_utils::move_file(input_path, &blob_path)
            .await
            .with_context(|| {
                format!(
                    "failed to move file from {} to {} to save blob",
                    input_path.display(),
                    blob_path.display()
                )
            })?;
        tracing::debug!(input_path = %input_path.display(), %blob_hash, ?move_type, "saved blob by moving file");
    } else {
        crate::fs_utils::atomic_copy(input_path, &blob_path)
            .await
            .with_context(|| {
                format!(
                    "failed to copy file from {} to {} to save blob",
                    input_path.display(),
                    blob_path.display()
                )
            })?;
        tokio::fs::set_permissions(&blob_path, permissions)
            .await
            .context("failed to set blob permissions")?;
        crate::fs_utils::set_mtime_to_brioche_epoch(input_path)
            .await
            .context("failed to set blob modified time")?;
        tracing::debug!(input_path = %input_path.display(), %blob_hash, "saved blob by copying file");

        if options.remove_input {
            tokio::fs::remove_file(input_path)
                .await
                .with_context(|| format!("failed to remove input file {}", input_path.display()))?;
        }
    }

    Ok(blob_hash)
}

#[derive(Default)]
pub struct SaveBlobOptions<'a> {
    expected_hash: Option<Hash>,
    expected_blob_hash: Option<BlobHash>,
    on_progress: Option<Box<dyn FnMut(usize) -> anyhow::Result<()> + Send + 'a>>,
    remove_input: bool,
}

impl<'a> SaveBlobOptions<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn expected_hash(mut self, expected_hash: Option<Hash>) -> Self {
        self.expected_hash = expected_hash;
        self
    }

    pub const fn expected_blob_hash(mut self, expected_blob_hash: Option<BlobHash>) -> Self {
        self.expected_blob_hash = expected_blob_hash;
        self
    }

    pub fn on_progress(
        mut self,
        on_progress: impl FnMut(usize) -> anyhow::Result<()> + Send + 'a,
    ) -> Self {
        self.on_progress = Some(Box::new(on_progress));
        self
    }

    pub const fn remove_input(mut self, remove_input: bool) -> Self {
        self.remove_input = remove_input;
        self
    }
}

pub async fn find_blob(brioche: &Brioche, hash: &Hash) -> anyhow::Result<Option<BlobHash>> {
    let hash_string = hash.to_string();
    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;
    let result = sqlx::query!(
        r#"
            SELECT blob_hash FROM blob_aliases WHERE hash = ? LIMIT 1
        "#,
        hash_string,
    )
    .fetch_optional(&mut *db_transaction)
    .await?;
    db_transaction.commit().await?;
    drop(db_conn);

    match result {
        Some(row) => {
            let blob_hash = row.blob_hash.parse()?;
            Ok(Some(blob_hash))
        }
        None => Ok(None),
    }
}

pub async fn blob_path(
    brioche: &Brioche,
    _permit: &mut SaveBlobPermit<'_>,
    blob_hash: BlobHash,
) -> anyhow::Result<PathBuf> {
    let local_path = local_blob_path(brioche, blob_hash);

    if tokio::fs::try_exists(&local_path).await? {
        return Ok(local_path);
    };

    if let Some(local_path_dir) = local_path.parent() {
        tokio::fs::create_dir_all(&local_path_dir).await?;
    }

    let blob = brioche.registry_client.get_blob(blob_hash).await?;

    let temp_dir = brioche.data_dir.join("blobs-temp");
    tokio::fs::create_dir_all(&temp_dir).await?;
    let temp_path = temp_dir.join(ulid::Ulid::new().to_string());

    let mut temp_file = tokio::fs::File::create(&temp_path)
        .await
        .context("failed to open temp file")?;
    temp_file
        .write_all(&blob)
        .await
        .context("failed to write blob to temp file")?;
    temp_file
        .set_permissions(blob_permissions())
        .await
        .context("failed to set blob permissions")?;
    let temp_file = temp_file.into_std().await;
    tokio::task::spawn_blocking(move || {
        temp_file.set_modified(crate::fs_utils::brioche_epoch())?;
        anyhow::Ok(())
    })
    .await??;

    tokio::fs::rename(&temp_path, &local_path)
        .await
        .context("failed to rename blob from temp file")?;

    Ok(local_path)
}

pub fn local_blob_path(brioche: &Brioche, blob_hash: BlobHash) -> PathBuf {
    let blobs_dir = brioche.data_dir.join("blobs");
    let blob_path = blobs_dir.join(hex::encode(blob_hash.0.as_bytes()));
    blob_path
}

fn blob_permissions() -> std::fs::Permissions {
    std::fs::Permissions::from_mode(0o444)
}

fn is_file_exclusive(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    metadata.nlink() == 1
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
pub struct BlobHash(blake3::Hash);

impl BlobHash {
    pub const fn from_blake3(hash: blake3::Hash) -> Self {
        Self(hash)
    }

    pub const fn to_blake3(&self) -> blake3::Hash {
        self.0
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    pub fn for_content(content: &[u8]) -> Self {
        let hash = blake3::hash(content);
        Self(hash)
    }

    pub fn validate_matches(&self, content: &[u8]) -> anyhow::Result<()> {
        let expected_hash = &self.0;
        let actual_hash = blake3::hash(content);
        anyhow::ensure!(
            expected_hash == &actual_hash,
            "blob does not match expected hash"
        );
        Ok(())
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
        write!(f, "{}", self.0.to_hex())
    }
}

impl std::str::FromStr for BlobHash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hash = blake3::Hash::from_hex(s)?;
        Ok(Self(hash))
    }
}

struct BlobHasher {
    hasher: blake3::Hasher,
    expected_blob_hash: Option<BlobHash>,
    validation_hash_with_hasher: Option<(super::Hash, super::Hasher)>,
}

impl BlobHasher {
    fn new(options: &SaveBlobOptions<'_>) -> Self {
        let hasher = blake3::Hasher::new();
        let validation_hash_with_hasher = options
            .expected_hash
            .as_ref()
            .map(|hash| (hash.clone(), super::Hasher::for_hash(hash)));

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

    fn finish(self) -> anyhow::Result<(BlobHash, Option<super::Hash>)> {
        let validated_hash =
            if let Some((expected_hash, validation_hasher)) = self.validation_hash_with_hasher {
                let actual_hash = validation_hasher.finish()?;

                if expected_hash != actual_hash {
                    anyhow::bail!("expected hash {expected_hash} but got {actual_hash}");
                }

                Some(actual_hash)
            } else {
                None
            };

        let hash = self.hasher.finalize();
        let blob_hash = BlobHash(hash);

        if let Some(expected_blob_hash) = self.expected_blob_hash {
            anyhow::ensure!(
                blob_hash == expected_blob_hash,
                "expected hash {expected_blob_hash} but got {blob_hash}"
            );
        }

        Ok((BlobHash(hash), validated_hash))
    }
}
