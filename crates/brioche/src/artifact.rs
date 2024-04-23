use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::Context as _;
use bstr::{BStr, BString};
use joinery::JoinableIterator as _;
use sqlx::{Acquire as _, Arguments as _};

use crate::encoding::TickEncoded;

use super::{blob::BlobHash, platform::Platform, Brioche, Hash};

#[serde_with::serde_as]
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum::EnumDiscriminants,
)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
#[strum_discriminants(vis(pub))]
#[strum_discriminants(derive(serde::Serialize, serde::Deserialize))]
#[strum_discriminants(serde(rename_all = "snake_case"))]
pub enum LazyArtifact {
    #[serde(rename_all = "camelCase")]
    File {
        content_blob: BlobHash,
        executable: bool,
        resources: Box<WithMeta<LazyArtifact>>,
    },
    #[serde(rename_all = "camelCase")]
    Directory(Directory),
    #[serde(rename_all = "camelCase")]
    Symlink {
        #[serde_as(as = "TickEncoded")]
        target: BString,
    },
    #[serde(rename_all = "camelCase")]
    Download(DownloadArtifact),
    #[serde(rename_all = "camelCase")]
    Unpack(UnpackArtifact),
    Process(ProcessArtifact),
    CompleteProcess(CompleteProcessArtifact),
    #[serde(rename_all = "camelCase")]
    CreateFile {
        #[serde_as(as = "TickEncoded")]
        content: BString,
        executable: bool,
        resources: Box<WithMeta<LazyArtifact>>,
    },
    #[serde(rename_all = "camelCase")]
    CreateDirectory(CreateDirectory),
    #[serde(rename_all = "camelCase")]
    Cast {
        artifact: Box<WithMeta<LazyArtifact>>,
        to: CompleteArtifactDiscriminants,
    },
    #[serde(rename_all = "camelCase")]
    Merge {
        directories: Vec<WithMeta<LazyArtifact>>,
    },
    #[serde(rename_all = "camelCase")]
    Peel {
        directory: Box<WithMeta<LazyArtifact>>,
        depth: u32,
    },
    #[serde(rename_all = "camelCase")]
    Get {
        directory: Box<WithMeta<LazyArtifact>>,
        #[serde_as(as = "TickEncoded")]
        path: BString,
    },
    #[serde(rename_all = "camelCase")]
    Insert {
        directory: Box<WithMeta<LazyArtifact>>,
        #[serde_as(as = "TickEncoded")]
        path: BString,
        artifact: Option<Box<WithMeta<LazyArtifact>>>,
    },
    #[serde(rename_all = "camelCase")]
    SetPermissions {
        file: Box<WithMeta<LazyArtifact>>,
        executable: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    Proxy(ProxyArtifact),
}

impl LazyArtifact {
    #[tracing::instrument(skip_all)]
    pub fn try_hash(&self) -> anyhow::Result<ArtifactHash> {
        static HASHES: OnceLock<RwLock<HashMap<LazyArtifact, ArtifactHash>>> = OnceLock::new();
        let hashes = HASHES.get_or_init(|| RwLock::new(HashMap::new()));
        {
            let hashes_reader = hashes
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire read lock on hashes"))?;
            if let Some(hash) = hashes_reader.get(self) {
                return Ok(*hash);
            }
        }

        let hash = ArtifactHash::from_serializable(self)?;
        {
            let mut hashes_writer = hashes
                .write()
                .map_err(|_| anyhow::anyhow!("failed to acquire write lock on hashes"))?;
            hashes_writer.insert(self.clone(), hash);
        }

        Ok(hash)
    }

    pub fn hash(&self) -> ArtifactHash {
        self.try_hash().expect("failed to hash artifact")
    }

    pub fn kind(&self) -> LazyArtifactDiscriminants {
        self.into()
    }

    pub fn is_expensive_to_resolve(&self) -> bool {
        match self {
            LazyArtifact::Download(_) | LazyArtifact::CompleteProcess(_) => true,
            LazyArtifact::File { .. }
            | LazyArtifact::Directory(_)
            | LazyArtifact::Symlink { .. }
            | LazyArtifact::Unpack(_)
            | LazyArtifact::Process(_)
            | LazyArtifact::CreateFile { .. }
            | LazyArtifact::CreateDirectory(_)
            | LazyArtifact::Cast { .. }
            | LazyArtifact::Merge { .. }
            | LazyArtifact::Peel { .. }
            | LazyArtifact::Get { .. }
            | LazyArtifact::Insert { .. }
            | LazyArtifact::SetPermissions { .. }
            | LazyArtifact::Proxy(_) => false,
        }
    }
}

pub async fn get_artifacts(
    brioche: &Brioche,
    artifact_hashes: impl IntoIterator<Item = ArtifactHash>,
) -> anyhow::Result<HashMap<ArtifactHash, LazyArtifact>> {
    let mut artifacts = HashMap::new();

    let cached_artifacts = brioche.cached_artifacts.read().await;
    let mut uncached_artifacts = HashSet::new();
    let mut arguments = sqlx::sqlite::SqliteArguments::default();

    for artifact_hash in artifact_hashes {
        match cached_artifacts.artifacts_by_hash.get(&artifact_hash) {
            Some(artifact) => {
                artifacts.insert(artifact_hash, artifact.clone());
            }
            None => {
                let is_new = uncached_artifacts.insert(artifact_hash);

                // Add as SQL argument unless we've added it before
                if is_new {
                    arguments.add(artifact_hash.to_string());
                }
            }
        }
    }

    // Release the lock
    drop(cached_artifacts);

    // Return early if we have no uncached artifacts to fetch
    if uncached_artifacts.is_empty() {
        return Ok(artifacts);
    }

    let placeholders = std::iter::repeat("?")
        .take(uncached_artifacts.len())
        .join_with(", ");

    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    let records = sqlx::query_as_with::<_, (String, String), _>(
        &format!(
            r#"
                SELECT artifact_hash, artifact_json
                FROM artifacts
                WHERE artifact_hash IN ({placeholders})
            "#,
        ),
        arguments,
    )
    .fetch_all(&mut *db_transaction)
    .await?;

    db_transaction.commit().await?;
    drop(db_conn);

    let mut cached_artifacts = brioche.cached_artifacts.write().await;
    for (artifact_hash, artifact_json) in records {
        let artifact: LazyArtifact = serde_json::from_str(&artifact_json)?;
        let expected_artifact_hash: ArtifactHash = artifact_hash.parse()?;
        let artifact_hash = artifact.hash();

        anyhow::ensure!(expected_artifact_hash == artifact_hash, "expected artifact hash from database to be {expected_artifact_hash}, but was {artifact_hash}");

        cached_artifacts
            .artifacts_by_hash
            .insert(artifact_hash, artifact.clone());
        uncached_artifacts.remove(&artifact_hash);
        artifacts.insert(artifact_hash, artifact);
    }

    if !uncached_artifacts.is_empty() {
        anyhow::bail!("artifacts not found: {uncached_artifacts:?}");
    }

    Ok(artifacts)
}

pub async fn get_artifact(
    brioche: &Brioche,
    artifact_hash: ArtifactHash,
) -> anyhow::Result<LazyArtifact> {
    let mut artifacts = get_artifacts(brioche, [artifact_hash]).await?;
    let artifact = artifacts
        .remove(&artifact_hash)
        .expect("artifact not returned in collection");
    Ok(artifact)
}

pub async fn save_artifacts<A>(
    brioche: &Brioche,
    artifacts: impl IntoIterator<Item = A>,
) -> anyhow::Result<u64>
where
    A: std::borrow::Borrow<LazyArtifact>,
{
    let cached_artifacts = brioche.cached_artifacts.read().await;

    let mut arguments = sqlx::sqlite::SqliteArguments::default();
    let mut uncached_artifacts = vec![];
    for artifact in artifacts {
        let artifact = artifact.borrow();
        let artifact_hash = artifact.hash();
        let artifact_json = serde_json::to_string(artifact)?;

        if !cached_artifacts
            .artifacts_by_hash
            .contains_key(&artifact_hash)
        {
            // Artifact not cached, so try to insert it into the database
            // and cache it afterward
            uncached_artifacts.push(artifact.clone());

            arguments.add(artifact_hash.to_string());
            arguments.add(artifact_json);
        }
    }

    // Release the read lock
    drop(cached_artifacts);

    // Short-circuit if we have no artifacts to save
    if uncached_artifacts.is_empty() {
        return Ok(0);
    }

    let placeholders = std::iter::repeat("(?, ?)")
        .take(uncached_artifacts.len())
        .join_with(", ");

    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    let result = sqlx::query_with(
        &format!(
            r#"
                INSERT INTO artifacts (artifact_hash, artifact_json)
                VALUES {placeholders}
                ON CONFLICT (artifact_hash) DO NOTHING
            "#
        ),
        arguments,
    )
    .execute(&mut *db_transaction)
    .await?;

    db_transaction.commit().await?;
    drop(db_conn);

    // Cache each artifact that wasn't cached before. We do this after
    // writing to the database to ensure cached items are always in the database
    let mut cached_artifacts = brioche.cached_artifacts.write().await;
    for artifact in uncached_artifacts {
        cached_artifacts
            .artifacts_by_hash
            .insert(artifact.hash(), artifact);
    }

    Ok(result.rows_affected())
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub source: Option<Vec<StackFrame>>,
}

#[derive(Debug, Clone)]
pub struct WithMeta<T> {
    pub value: T,
    pub meta: Arc<Meta>,
}

impl<T> WithMeta<T> {
    pub fn new(value: T, meta: Arc<Meta>) -> Self {
        Self { value, meta }
    }

    pub fn without_meta(value: T) -> Self {
        Self {
            value,
            meta: Arc::new(Meta::default()),
        }
    }

    pub fn as_ref(&self) -> WithMeta<&T> {
        WithMeta {
            value: &self.value,
            meta: self.meta.clone(),
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> WithMeta<U> {
        WithMeta {
            value: f(self.value),
            meta: self.meta,
        }
    }

    pub fn source_frame(&self) -> Option<&StackFrame> {
        self.meta.source.as_ref().and_then(|frames| frames.first())
    }
}

impl<T> serde::Serialize for WithMeta<T>
where
    T: serde::Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.value, serializer)
    }
}

impl<'de, T> serde::Deserialize<'de> for WithMeta<T>
where
    T: serde::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value: T = serde::Deserialize::deserialize(deserializer)?;
        Ok(WithMeta::without_meta(value))
    }
}

impl<T, U> std::cmp::PartialEq<WithMeta<U>> for WithMeta<T>
where
    T: PartialEq<U>,
{
    fn eq(&self, other: &WithMeta<U>) -> bool {
        self.value == other.value
    }
}

impl<T> std::cmp::Eq for WithMeta<T> where T: Eq {}

impl<T> std::hash::Hash for WithMeta<T>
where
    T: std::hash::Hash,
{
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl std::ops::Deref for WithMeta<LazyArtifact> {
    type Target = LazyArtifact;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl std::ops::DerefMut for WithMeta<LazyArtifact> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl std::ops::Deref for WithMeta<CompleteArtifact> {
    type Target = CompleteArtifact;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl std::ops::DerefMut for WithMeta<CompleteArtifact> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl std::ops::Deref for WithMeta<ArtifactHash> {
    type Target = ArtifactHash;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl std::ops::DerefMut for WithMeta<ArtifactHash> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub file_name: Option<String>,
    pub line_number: Option<i64>,
    pub column_number: Option<i64>,
}

impl std::fmt::Display for StackFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let file_name = self.file_name.as_deref().unwrap_or("<unknown>");
        match (self.line_number, self.column_number) {
            (Some(line), Some(column)) => {
                write!(f, "{file_name}:{}:{}", line, column)
            }
            (Some(line), None) => {
                write!(f, "{file_name}:{}", line)
            }
            (None, _) => {
                write!(f, "{file_name}")
            }
        }
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDirectory {
    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    pub entries: BTreeMap<BString, WithMeta<LazyArtifact>>,
}

impl CreateDirectory {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadArtifact {
    pub url: url::Url,
    pub hash: Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnpackArtifact {
    pub file: Box<WithMeta<LazyArtifact>>,
    pub archive: ArchiveFormat,
    #[serde(default)]
    pub compression: CompressionFormat,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessArtifact {
    pub command: ProcessTemplate,

    pub args: Vec<ProcessTemplate>,

    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    pub env: BTreeMap<BString, ProcessTemplate>,

    pub work_dir: Box<WithMeta<LazyArtifact>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_scaffold: Option<Box<WithMeta<LazyArtifact>>>,

    pub platform: Platform,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteProcessArtifact {
    pub command: CompleteProcessTemplate,

    pub args: Vec<CompleteProcessTemplate>,

    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    pub env: BTreeMap<BString, CompleteProcessTemplate>,

    #[serde_as(as = "serde_with::TryFromInto<LazyArtifact>")]
    pub work_dir: Directory,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_scaffold: Option<Box<CompleteArtifact>>,

    pub platform: Platform,
}

#[serde_with::serde_as]
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum::EnumDiscriminants,
)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
#[strum_discriminants(vis(pub))]
#[strum_discriminants(derive(Hash, serde::Serialize, serde::Deserialize))]
#[strum_discriminants(serde(rename_all = "snake_case"))]
pub enum CompleteArtifact {
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

impl CompleteArtifact {
    #[tracing::instrument(skip_all)]
    pub fn try_hash(&self) -> anyhow::Result<ArtifactHash> {
        let hash = ArtifactHash::from_serializable(self)?;
        Ok(hash)
    }

    pub fn hash(&self) -> ArtifactHash {
        self.try_hash().expect("failed to hash artifact")
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub content_blob: BlobHash,

    pub executable: bool,

    #[serde_as(as = "serde_with::TryFromInto<LazyArtifact>")]
    pub resources: Directory,
}

#[serde_with::serde_as]
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    entries: BTreeMap<BString, WithMeta<ArtifactHash>>,
}

impl Directory {
    pub async fn create(
        brioche: &Brioche,
        entries: &BTreeMap<BString, WithMeta<CompleteArtifact>>,
    ) -> anyhow::Result<Self> {
        let artifacts = entries
            .values()
            .map(|artifact| LazyArtifact::from(artifact.value.clone()));
        save_artifacts(brioche, artifacts).await?;

        let entries = entries
            .iter()
            .map(|(path, artifact)| {
                let artifact_hash = artifact.as_ref().map(|artifact| artifact.hash());
                (path.clone(), artifact_hash)
            })
            .collect();
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub async fn entries(
        &self,
        brioche: &Brioche,
    ) -> anyhow::Result<BTreeMap<BString, WithMeta<CompleteArtifact>>> {
        let entry_artifacts =
            get_artifacts(brioche, self.entries.values().map(|entry| **entry)).await?;

        let entries = self
            .entries
            .iter()
            .map(|(path, artifact_hash)| {
                let artifact = entry_artifacts
                    .get(&**artifact_hash)
                    .with_context(|| format!("failed to get artifact for entry {path:?}"))?;
                let artifact: CompleteArtifact = artifact.clone().try_into().map_err(|_| {
                    anyhow::anyhow!("artifact at {path:?} is not a complete artifact")
                })?;
                let artifact = artifact_hash.as_ref().map(|_| artifact);
                Ok((path.clone(), artifact))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(entries)
    }

    #[async_recursion::async_recursion]
    async fn get_by_components(
        &self,
        brioche: &Brioche,
        full_path: &BStr,
        path_components: &[&BStr],
    ) -> Result<Option<WithMeta<CompleteArtifact>>, DirectoryError> {
        match path_components {
            [] => Err(DirectoryError::EmptyPath {
                path: full_path.into(),
            }),
            [filename] => match self.entries.get(&**filename) {
                Some(artifact_hash) => {
                    let artifact = get_artifact(brioche, artifact_hash.value).await?;
                    let artifact: CompleteArtifact =
                        artifact
                            .try_into()
                            .map_err(|_| DirectoryError::ArtifactIncomplete {
                                path: full_path.into(),
                            })?;
                    Ok(Some(artifact_hash.as_ref().map(|_| artifact)))
                }
                None => Ok(None),
            },
            [directory_name, path_components @ ..] => {
                let Some(dir_entry_hash) = self.entries.get(&**directory_name) else {
                    return Ok(None);
                };
                let dir_entry = get_artifact(brioche, dir_entry_hash.value).await?;
                let dir_entry: CompleteArtifact =
                    dir_entry
                        .try_into()
                        .map_err(|_| DirectoryError::ArtifactIncomplete {
                            path: full_path.into(),
                        })?;
                let CompleteArtifact::Directory(dir_entry) = dir_entry else {
                    return Err(DirectoryError::PathDescendsIntoNonDirectory {
                        path: full_path.into(),
                    });
                };
                let artifact = dir_entry
                    .get_by_components(brioche, full_path, path_components)
                    .await?;
                Ok(artifact)
            }
        }
    }

    #[async_recursion::async_recursion]
    async fn insert_by_components(
        &mut self,
        brioche: &Brioche,
        full_path: &BStr,
        path_components: &[&BStr],
        artifact: Option<WithMeta<CompleteArtifact>>,
    ) -> Result<Option<WithMeta<CompleteArtifact>>, DirectoryError> {
        match path_components {
            [] => {
                return Err(DirectoryError::EmptyPath {
                    path: full_path.into(),
                });
            }
            [filename] => {
                let replaced_hash = match artifact {
                    Some(artifact) => {
                        let artifact_hash = artifact.as_ref().map(|artifact| artifact.hash());
                        save_artifacts(brioche, [LazyArtifact::from(artifact.value)]).await?;
                        self.entries.insert(filename.to_vec().into(), artifact_hash)
                    }
                    None => self.entries.remove(&**filename),
                };
                let replaced = match replaced_hash {
                    Some(artifact_hash) => {
                        let artifact = get_artifact(brioche, *artifact_hash).await?;
                        let artifact: CompleteArtifact = artifact.try_into().map_err(|_| {
                            DirectoryError::ArtifactIncomplete {
                                path: full_path.into(),
                            }
                        })?;
                        Some(artifact_hash.map(|_| artifact))
                    }
                    None => None,
                };
                Ok(replaced)
            }
            [directory_name, path_components @ ..] => {
                let replaced = match self.entries.entry(directory_name.to_vec().into()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let mut new_directory = Directory::default();
                        new_directory
                            .insert_by_components(brioche, full_path, path_components, artifact)
                            .await?;

                        let new_directory: LazyArtifact = new_directory.into();
                        let new_directory_hash = new_directory.hash();
                        save_artifacts(brioche, [new_directory]).await?;

                        entry.insert(WithMeta::without_meta(new_directory_hash));

                        None
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let dir_entry_hash = entry.get();

                        let dir_entry = get_artifact(brioche, **dir_entry_hash).await?;
                        let dir_entry: CompleteArtifact = dir_entry.try_into().map_err(|_| {
                            DirectoryError::ArtifactIncomplete {
                                path: full_path.into(),
                            }
                        })?;
                        let CompleteArtifact::Directory(mut inner_dir) = dir_entry else {
                            return Err(DirectoryError::PathDescendsIntoNonDirectory {
                                path: full_path.into(),
                            });
                        };
                        let replaced = inner_dir
                            .insert_by_components(brioche, full_path, path_components, artifact)
                            .await?;

                        let updated_dir_entry: LazyArtifact = inner_dir.into();
                        let updated_dir_entry_hash = updated_dir_entry.hash();
                        save_artifacts(brioche, [updated_dir_entry]).await?;
                        entry.insert(WithMeta::without_meta(updated_dir_entry_hash));

                        replaced
                    }
                };
                Ok(replaced)
            }
        }
    }

    pub async fn get(
        &self,
        brioche: &Brioche,
        path: &[u8],
    ) -> Result<Option<WithMeta<CompleteArtifact>>, DirectoryError> {
        let path = bstr::BStr::new(path);
        let mut components = vec![];
        for component in path.split(|&byte| byte == b'/' || byte == b'\\') {
            if component.is_empty() || component == b"." {
                // Skip this component
            } else if component == b".." {
                // Pop the last component
                let removed_component = components.pop();
                if removed_component.is_none() {
                    return Err(DirectoryError::PathEscapes { path: path.into() });
                }
            } else {
                // Push this component
                components.push(bstr::BStr::new(component));
            }
        }

        self.get_by_components(brioche, path, &components).await
    }

    pub async fn insert(
        &mut self,
        brioche: &Brioche,
        path: &[u8],
        artifact: Option<WithMeta<CompleteArtifact>>,
    ) -> Result<Option<WithMeta<CompleteArtifact>>, DirectoryError> {
        let path = bstr::BStr::new(path);
        let mut components = vec![];
        for component in path.split(|&byte| byte == b'/' || byte == b'\\') {
            if component.is_empty() || component == b"." {
                // Skip this component
            } else if component == b".." {
                // Pop the last component
                let removed_component = components.pop();
                if removed_component.is_none() {
                    return Err(DirectoryError::PathEscapes { path: path.into() });
                }
            } else {
                // Push this component
                components.push(bstr::BStr::new(component));
            }
        }

        self.insert_by_components(brioche, path, &components, artifact)
            .await
    }

    #[async_recursion::async_recursion]
    pub async fn merge(&mut self, other: &Self, brioche: &Brioche) -> anyhow::Result<()> {
        for (key, artifact) in &other.entries {
            match self.entries.entry(key.clone()) {
                std::collections::btree_map::Entry::Occupied(mut current) => {
                    let (current_dir_entry, other_dir_entry) = tokio::try_join!(
                        get_artifact(brioche, **current.get()),
                        get_artifact(brioche, **artifact),
                    )?;

                    let current_dir_entry: CompleteArtifact =
                        current_dir_entry.try_into().map_err(|_| {
                            anyhow::anyhow!("current artifact at {key:?} is not complete")
                        })?;
                    let other_dir_entry: CompleteArtifact =
                        other_dir_entry.try_into().map_err(|_| {
                            anyhow::anyhow!("other artifact at {key:?} is not complete")
                        })?;
                    match (current_dir_entry, other_dir_entry) {
                        (
                            CompleteArtifact::Directory(mut current_inner),
                            CompleteArtifact::Directory(other_inner),
                        ) => {
                            current_inner.merge(&other_inner, brioche).await?;

                            let updated_current_inner_artifact: LazyArtifact = current_inner.into();
                            let updated_current_inner_hash = updated_current_inner_artifact.hash();
                            save_artifacts(brioche, [updated_current_inner_artifact]).await?;
                            current.insert(WithMeta::without_meta(updated_current_inner_hash));
                        }
                        (_, other_dir_entry) => {
                            current.insert(artifact.as_ref().map(|_| other_dir_entry.hash()));
                        }
                    }
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(artifact.clone());
                }
            }
        }

        Ok(())
    }
}

impl TryFrom<LazyArtifact> for CompleteArtifact {
    type Error = ArtifactIncomplete;

    fn try_from(value: LazyArtifact) -> Result<Self, Self::Error> {
        match value {
            LazyArtifact::File {
                content_blob: data,
                executable,
                resources,
            } => {
                let resources: CompleteArtifact = resources.value.try_into()?;
                let CompleteArtifact::Directory(resources) = resources else {
                    return Err(ArtifactIncomplete);
                };
                Ok(CompleteArtifact::File(File {
                    content_blob: data,
                    executable,
                    resources,
                }))
            }
            LazyArtifact::Symlink { target } => Ok(CompleteArtifact::Symlink { target }),
            LazyArtifact::Directory(directory) => Ok(CompleteArtifact::Directory(directory)),
            LazyArtifact::CreateDirectory(directory) if directory.is_empty() => {
                Ok(CompleteArtifact::Directory(Directory::default()))
            }
            LazyArtifact::Download { .. }
            | LazyArtifact::Unpack { .. }
            | LazyArtifact::Process { .. }
            | LazyArtifact::CompleteProcess { .. }
            | LazyArtifact::CreateFile { .. }
            | LazyArtifact::CreateDirectory(..)
            | LazyArtifact::Cast { .. }
            | LazyArtifact::Merge { .. }
            | LazyArtifact::Peel { .. }
            | LazyArtifact::Get { .. }
            | LazyArtifact::Insert { .. }
            | LazyArtifact::SetPermissions { .. }
            | LazyArtifact::Proxy { .. } => Err(ArtifactIncomplete),
        }
    }
}

impl From<CompleteArtifact> for LazyArtifact {
    fn from(value: CompleteArtifact) -> Self {
        match value {
            CompleteArtifact::File(File {
                content_blob: data,
                executable,
                resources,
            }) => Self::File {
                content_blob: data,
                executable,
                resources: Box::new(WithMeta::without_meta(LazyArtifact::Directory(resources))),
            },
            CompleteArtifact::Symlink { target } => Self::Symlink { target },
            CompleteArtifact::Directory(directory) => Self::Directory(directory),
        }
    }
}

impl From<Directory> for LazyArtifact {
    fn from(value: Directory) -> Self {
        Self::Directory(value)
    }
}

impl TryFrom<LazyArtifact> for Directory {
    type Error = anyhow::Error;

    fn try_from(value: LazyArtifact) -> Result<Self, Self::Error> {
        match value {
            LazyArtifact::Directory(directory) => Ok(directory),
            _ => {
                anyhow::bail!("expected directory artifact");
            }
        }
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyArtifact {
    pub artifact: ArtifactHash,
}

impl ProxyArtifact {
    pub async fn inner(&self, brioche: &Brioche) -> anyhow::Result<LazyArtifact> {
        let inner = get_artifact(brioche, self.artifact).await?;
        Ok(inner)
    }
}

pub struct ArtifactIncomplete;

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("empty path: {path:?}")]
    EmptyPath { path: bstr::BString },
    #[error("path escapes directory structure: {path:?}")]
    PathEscapes { path: bstr::BString },
    #[error("path descends into non-directory: {path:?}")]
    PathDescendsIntoNonDirectory { path: bstr::BString },
    #[error("path {path:?} contains a non-complete artifact")]
    ArtifactIncomplete { path: bstr::BString },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
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
pub struct ArtifactHash(blake3::Hash);

impl ArtifactHash {
    fn from_serializable<V>(value: &V) -> anyhow::Result<Self>
    where
        V: serde::Serialize,
    {
        let mut hasher = blake3::Hasher::new();

        json_canon::to_writer(&mut hasher, value)?;

        let hash = hasher.finalize();
        Ok(Self(hash))
    }
}

impl std::fmt::Display for ArtifactHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ArtifactHash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hash = blake3::Hash::from_hex(s)?;
        Ok(Self(hash))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessTemplate {
    pub components: Vec<ProcessTemplateComponent>,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ProcessTemplateComponent {
    Literal {
        #[serde_as(as = "TickEncoded")]
        value: BString,
    },
    Input {
        artifact: WithMeta<LazyArtifact>,
    },
    OutputPath,
    ResourcesDir,
    HomeDir,
    WorkDir,
    TempDir,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteProcessTemplate {
    pub components: Vec<CompleteProcessTemplateComponent>,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum CompleteProcessTemplateComponent {
    Literal {
        #[serde_as(as = "TickEncoded")]
        value: BString,
    },
    Input {
        artifact: WithMeta<CompleteArtifact>,
    },
    OutputPath,
    ResourcesDir,
    HomeDir,
    WorkDir,
    TempDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Tar,
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CompressionFormat {
    #[default]
    None,
    Bzip2,
    Gzip,
    Xz,
    Zstd,
}

impl CompressionFormat {
    pub fn decompress(
        &self,
        input: impl tokio::io::AsyncBufRead + Unpin + Send + 'static,
    ) -> Box<dyn tokio::io::AsyncRead + Unpin + Send> {
        match self {
            Self::None => Box::new(input),
            Self::Bzip2 => Box::new(async_compression::tokio::bufread::BzDecoder::new(input)),
            Self::Gzip => Box::new(async_compression::tokio::bufread::GzipDecoder::new(input)),
            Self::Xz => Box::new(async_compression::tokio::bufread::XzDecoder::new(input)),
            Self::Zstd => Box::new(async_compression::tokio::bufread::ZstdDecoder::new(input)),
        }
    }
}
