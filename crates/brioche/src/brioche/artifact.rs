use std::{
    collections::{BTreeMap, HashMap},
    io::Write as _,
    sync::{Arc, OnceLock, RwLock},
};

use bstr::BString;

use crate::encoding::UrlEncoded;

use super::{blob::BlobId, platform::Platform, Hash};

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
        data: BlobId,
        executable: bool,
        resources: LazyDirectory,
    },
    #[serde(rename_all = "camelCase")]
    Symlink {
        #[serde_as(as = "UrlEncoded")]
        target: BString,
    },
    #[serde(rename_all = "camelCase")]
    Directory(LazyDirectory),
    #[serde(rename_all = "camelCase")]
    Download(DownloadArtifact),
    #[serde(rename_all = "camelCase")]
    Unpack(UnpackArtifact),
    Process(ProcessArtifact),
    CompleteProcess(CompleteProcessArtifact),
    #[serde(rename_all = "camelCase")]
    CreateFile {
        #[serde_as(as = "UrlEncoded")]
        data: BString,
        executable: bool,
        resources: Box<WithMeta<LazyArtifact>>,
    },
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
        #[serde_as(as = "UrlEncoded")]
        path: BString,
    },
    #[serde(rename_all = "camelCase")]
    Remove {
        directory: Box<WithMeta<LazyArtifact>>,
        #[serde_as(as = "Vec<UrlEncoded>")]
        paths: Vec<BString>,
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

#[derive(Debug, Clone, serde::Deserialize)]
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

// TODO: This manual impl is a workaround because bincode doesn't support
// `#[serde(flatten)]`. We should either use the bincode derive macros
// or see if we can elimiinate this manual impl some other way.
impl<T> serde::Serialize for WithMeta<T>
where
    T: serde::Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.value.serialize(serializer)
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
pub struct LazyDirectory {
    #[serde_as(as = "BTreeMap<UrlEncoded, _>")]
    pub entries: BTreeMap<BString, WithMeta<LazyArtifact>>,
}

impl LazyDirectory {
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
    #[serde_as(as = "BTreeMap<UrlEncoded, _>")]
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
    #[serde_as(as = "BTreeMap<UrlEncoded, _>")]
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
        #[serde_as(as = "UrlEncoded")]
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
    pub data: BlobId,
    pub executable: bool,
    pub resources: Directory,
}

#[serde_with::serde_as]
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    #[serde_as(as = "BTreeMap<UrlEncoded, _>")]
    pub entries: BTreeMap<BString, WithMeta<CompleteArtifact>>,
}

impl Directory {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(
        &mut self,
        path: &[u8],
        artifact: WithMeta<CompleteArtifact>,
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

        let Some((filename, path_components)) = components.split_last() else {
            return Err(DirectoryError::EmptyPath { path: path.into() });
        };

        let mut directory = self;
        for component in path_components {
            let entry = directory
                .entries
                .entry(component.to_vec().into())
                .or_insert_with(|| {
                    WithMeta::without_meta(CompleteArtifact::Directory(Directory::default()))
                });
            let CompleteArtifact::Directory(entry) = &mut entry.value else {
                return Err(DirectoryError::PathDescendsIntoNonDirectory { path: path.into() });
            };
            directory = entry;
        }

        let replaced = directory.entries.insert(filename.to_vec().into(), artifact);

        Ok(replaced)
    }

    pub fn get(&self, path: &[u8]) -> anyhow::Result<Option<&WithMeta<CompleteArtifact>>> {
        let path = bstr::BStr::new(path);
        let mut components = vec![];
        for component in path.split(|&byte| byte == b'/' || byte == b'\\') {
            if component.is_empty() || component == b"." {
                // Skip this component
            } else if component == b".." {
                // Pop the last component
                let removed_component = components.pop();
                anyhow::ensure!(
                    removed_component.is_none(),
                    "path escapes outside directory structure"
                );
            } else {
                // Push this component
                components.push(bstr::BStr::new(component));
            }
        }

        let Some((filename, path_components)) = components.split_last() else {
            anyhow::bail!("empty path");
        };

        let mut directory = self;
        for component in path_components {
            let entry = directory.entries.get(&**component);
            let entry = match entry {
                Some(entry) => entry,
                None => {
                    return Ok(None);
                }
            };
            let entry = match &entry.value {
                CompleteArtifact::Directory(directory) => directory,
                other => anyhow::bail!(
                    "tried to descend into non-directory at {}: {other:?}",
                    BString::from(path),
                ),
            };
            directory = entry;
        }

        let found = directory.entries.get(&**filename);

        Ok(found)
    }

    pub fn remove(
        &mut self,
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

        let Some((filename, path_components)) = components.split_last() else {
            return Err(DirectoryError::EmptyPath { path: path.into() });
        };

        let mut directory = self;
        for component in path_components {
            let Some(entry) = directory.entries.get_mut(&**component) else {
                return Ok(None);
            };
            let CompleteArtifact::Directory(entry) = &mut entry.value else {
                return Err(DirectoryError::PathDescendsIntoNonDirectory { path: path.into() });
            };
            directory = entry;
        }

        let removed = directory.entries.remove(&**filename);

        Ok(removed)
    }

    pub fn merge(&mut self, other: Directory) {
        for (key, artifact) in other.entries {
            match self.entries.entry(key) {
                std::collections::btree_map::Entry::Occupied(current) => {
                    match (&mut current.into_mut().value, artifact.value) {
                        (
                            CompleteArtifact::Directory(current),
                            CompleteArtifact::Directory(other),
                        ) => {
                            current.merge(other);
                        }
                        (current, artifact) => {
                            *current = artifact;
                        }
                    }
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(artifact);
                }
            }
        }
    }
}

impl TryFrom<LazyArtifact> for CompleteArtifact {
    type Error = ArtifactIncomplete;

    fn try_from(value: LazyArtifact) -> Result<Self, Self::Error> {
        match value {
            LazyArtifact::File {
                data,
                executable,
                resources: pack,
            } => Ok(CompleteArtifact::File(File {
                data,
                executable,
                resources: pack.try_into()?,
            })),
            LazyArtifact::Symlink { target } => Ok(CompleteArtifact::Symlink { target }),
            LazyArtifact::Directory(directory) => {
                Ok(CompleteArtifact::Directory(directory.try_into()?))
            }
            LazyArtifact::Download { .. }
            | LazyArtifact::Unpack { .. }
            | LazyArtifact::Process { .. }
            | LazyArtifact::CompleteProcess { .. }
            | LazyArtifact::CreateFile { .. }
            | LazyArtifact::Cast { .. }
            | LazyArtifact::Merge { .. }
            | LazyArtifact::Peel { .. }
            | LazyArtifact::Get { .. }
            | LazyArtifact::Remove { .. }
            | LazyArtifact::SetPermissions { .. }
            | LazyArtifact::Proxy { .. } => Err(ArtifactIncomplete),
        }
    }
}

impl From<CompleteArtifact> for LazyArtifact {
    fn from(value: CompleteArtifact) -> Self {
        match value {
            CompleteArtifact::File(File {
                data,
                executable,
                resources,
            }) => Self::File {
                data,
                executable,
                resources: resources.into(),
            },
            CompleteArtifact::Symlink { target } => Self::Symlink { target },
            CompleteArtifact::Directory(directory) => Self::Directory(directory.into()),
        }
    }
}

impl From<Directory> for LazyArtifact {
    fn from(value: Directory) -> Self {
        let entries = value
            .entries
            .into_iter()
            .map(|(name, entry)| {
                let entry = WithMeta::new(LazyArtifact::from(entry.value), entry.meta);
                (name, entry)
            })
            .collect();
        Self::Directory(LazyDirectory { entries })
    }
}

impl TryFrom<LazyDirectory> for Directory {
    type Error = ArtifactIncomplete;

    fn try_from(value: LazyDirectory) -> Result<Self, Self::Error> {
        let entries = value
            .entries
            .into_iter()
            .map(|(name, entry)| {
                let entry_artifact: CompleteArtifact =
                    entry.value.try_into().map_err(|_| ArtifactIncomplete)?;
                let entry = WithMeta::new(entry_artifact, entry.meta);
                Result::<_, ArtifactIncomplete>::Ok((name, entry))
            })
            .collect::<Result<BTreeMap<_, _>, ArtifactIncomplete>>()?;
        Ok(Directory { entries })
    }
}

impl From<Directory> for LazyDirectory {
    fn from(value: Directory) -> Self {
        let entries = value
            .entries
            .into_iter()
            .map(|(name, entry)| {
                let entry = WithMeta::new(LazyArtifact::from(entry.value), entry.meta);
                (name, entry)
            })
            .collect();
        Self { entries }
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
    /// A common prefix used when hasing an artifact. When incompatible changes
    /// are made to the artifact schema, this prefix will be changed to ensure
    /// that we don't accidentally match a former artifact hash that has the
    /// same binary representation.
    const VERSION_PREFIX: &'static [u8] = b"v0              ";

    fn from_serializable<V>(value: &V) -> anyhow::Result<Self>
    where
        V: serde::Serialize,
    {
        let mut hasher = blake3::Hasher::new();
        hasher.write_all(Self::VERSION_PREFIX)?;

        let mut serializer = bincode::Serializer::new(&mut hasher, bincode::options());
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
        #[serde_as(as = "UrlEncoded")]
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
        #[serde_as(as = "UrlEncoded")]
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
