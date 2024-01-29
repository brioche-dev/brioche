use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, OnceLock, RwLock},
};

use bstr::{BStr, BString};
use serde::Serialize;

use crate::encoding::TickEncoded;

use super::{
    blob::{self, BlobId},
    platform::Platform,
    Brioche, Hash,
};

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
        content_blob: BlobId,
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
    Proxy {
        hash: ArtifactHash,
    },
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
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub source: Option<Vec<StackFrame>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WithMeta<T> {
    #[serde(flatten)]
    pub value: T,
    #[serde(default, skip_serializing)]
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

    pub fn source_frame(&self) -> Option<&StackFrame> {
        self.meta.source.as_ref().and_then(|frames| frames.first())
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
    pub work_dir: Directory,
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
    pub content_blob: BlobId,
    pub executable: bool,
    pub resources: Box<CompleteArtifact>,
}

#[serde_with::serde_as]
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    pub listing_blob: Option<BlobId>,
}

impl Directory {
    pub async fn create(brioche: &Brioche, listing: &DirectoryListing) -> anyhow::Result<Self> {
        if listing.is_empty() {
            return Ok(Self::default());
        }

        let mut listing_json = vec![];
        let mut serializer = serde_json::Serializer::with_formatter(
            &mut listing_json,
            olpc_cjson::CanonicalFormatter::new(),
        );
        listing.serialize(&mut serializer)?;

        let listing_blob =
            blob::save_blob(brioche, &listing_json, blob::SaveBlobOptions::default()).await?;

        Ok(Self {
            listing_blob: Some(listing_blob),
        })
    }

    pub async fn listing(&self, brioche: &Brioche) -> anyhow::Result<DirectoryListing> {
        match self.listing_blob {
            Some(tree_blob_id) => {
                let blob_path = blob::blob_path(brioche, tree_blob_id);
                let listing_json = tokio::fs::read(&blob_path).await?;
                let listing = serde_json::from_slice(&listing_json)?;
                Ok(listing)
            }
            None => Ok(DirectoryListing::default()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.listing_blob.is_none()
    }
}

#[serde_with::serde_as]
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    pub entries: BTreeMap<BString, WithMeta<CompleteArtifact>>,
}

impl DirectoryListing {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
            [filename] => Ok(self.entries.get(&**filename).cloned()),
            [directory_name, path_components @ ..] => {
                let Some(dir_entry) = self.entries.get(&**directory_name) else {
                    return Ok(None);
                };
                let CompleteArtifact::Directory(dir_entry) = &dir_entry.value else {
                    return Err(DirectoryError::PathDescendsIntoNonDirectory {
                        path: full_path.into(),
                    });
                };
                let listing = dir_entry.listing(brioche).await?;
                let artifact = listing
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
                let replaced = match artifact {
                    Some(artifact) => self.entries.insert(filename.to_vec().into(), artifact),
                    None => self.entries.remove(&**filename),
                };
                Ok(replaced)
            }
            [directory_name, path_components @ ..] => {
                let replaced = match self.entries.entry(directory_name.to_vec().into()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let mut new_listing = DirectoryListing::default();
                        let replaced = new_listing
                            .insert_by_components(brioche, full_path, path_components, artifact)
                            .await?;
                        let new_directory = Directory::create(brioche, &new_listing).await?;
                        entry.insert(WithMeta::without_meta(CompleteArtifact::Directory(
                            new_directory,
                        )));

                        replaced
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let mut listing = {
                            let CompleteArtifact::Directory(dir_entry) = &entry.get().value else {
                                return Err(DirectoryError::PathDescendsIntoNonDirectory {
                                    path: full_path.into(),
                                });
                            };
                            dir_entry.listing(brioche).await?
                        };
                        let replaced = listing
                            .insert_by_components(brioche, full_path, path_components, artifact)
                            .await?;
                        let updated_directory = Directory::create(brioche, &listing).await?;
                        entry.insert(WithMeta::without_meta(CompleteArtifact::Directory(
                            updated_directory,
                        )));

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
                std::collections::btree_map::Entry::Occupied(current) => {
                    match (&mut current.into_mut().value, &artifact.value) {
                        (
                            CompleteArtifact::Directory(current),
                            CompleteArtifact::Directory(other),
                        ) => {
                            let (mut current_listing, other_listing) =
                                tokio::try_join!(current.listing(brioche), other.listing(brioche))?;

                            current_listing.merge(&other_listing, brioche).await?;
                            *current = Directory::create(brioche, &current_listing).await?;
                        }
                        (current, artifact) => {
                            *current = artifact.clone();
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
                    resources: Box::new(CompleteArtifact::Directory(resources)),
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
                resources: Box::new(WithMeta::without_meta(LazyArtifact::from(*resources))),
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

pub struct ArtifactIncomplete;

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("empty path: {path:?}")]
    EmptyPath { path: bstr::BString },
    #[error("path escapes directory structure: {path:?}")]
    PathEscapes { path: bstr::BString },
    #[error("path descends into non-directory: {path:?}")]
    PathDescendsIntoNonDirectory { path: bstr::BString },
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

        let mut serializer = serde_json::Serializer::with_formatter(
            &mut hasher,
            olpc_cjson::CanonicalFormatter::new(),
        );
        value.serialize(&mut serializer)?;

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
            Self::Gzip => Box::new(async_compression::tokio::bufread::GzipDecoder::new(input)),
            Self::Xz => Box::new(async_compression::tokio::bufread::XzDecoder::new(input)),
            Self::Zstd => Box::new(async_compression::tokio::bufread::ZstdDecoder::new(input)),
        }
    }
}
