use std::collections::{BTreeMap, BTreeSet};

use bstr::BString;

use crate::{blob::BlobHash, hash::AnyHash, platform::Platform};

mod graph;
pub mod hash;

pub use graph::RecipeRef;

#[derive(Default)]
pub struct Recipes {
    graph: graph::RecipeGraph,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    File(File),
    Directory(Directory),
    Symlink(Symlink),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub content_blob: BlobHash,
    pub executable: bool,
    pub resources: Option<RecipeRef>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Directory {
    pub entries: BTreeMap<BString, RecipeRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symlink {
    pub target: BString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipe {
    File(File),
    Directory(Directory),
    Symlink(Symlink),
    Download(DownloadRecipe),
    Unarchive(UnarchiveRecipe),
    Process(ProcessRecipe),
    CompleteProcess(CompleteProcessRecipe),
    CreateFile {
        content: BString,
        executable: bool,
        resources: Option<RecipeRef>,
    },
    CreateDirectory {
        entries: BTreeMap<BString, RecipeRef>,
    },
    Cast {
        recipe: RecipeRef,
        to: ArtifactKind,
    },
    Merge {
        directories: Vec<RecipeRef>,
    },
    Peel {
        directory: RecipeRef,
        depth: u32,
    },
    Get {
        directory: RecipeRef,
        path: BString,
    },
    Insert {
        directory: RecipeRef,
        path: BString,
        recipe: Option<RecipeRef>,
    },
    Glob {
        directory: RecipeRef,
        patterns: BTreeSet<BString>,
    },
    SetPermissions {
        file: RecipeRef,
        executable: Option<bool>,
    },
    CollectReferences {
        recipe: RecipeRef,
    },
    AttachResources {
        recipe: RecipeRef,
    },
    Proxy {
        recipe: RecipeRef,
    },
    Sync {
        recipe: RecipeRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRecipe {
    pub url: url::Url,
    pub hash: AnyHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnarchiveRecipe {
    pub file: RecipeRef,
    pub archive: ArchiveFormat,
    pub compression: CompressionFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRecipe {
    pub command: ProcessTemplate,
    pub args: Vec<ProcessTemplate>,
    pub env: BTreeMap<BString, ProcessTemplate>,
    pub current_dir: ProcessTemplate,
    pub dependencies: Vec<RecipeRef>,
    pub work_dir: RecipeRef,
    pub output_scaffold: Option<RecipeRef>,
    pub platform: Platform,
    pub is_unsafe: bool,
    pub networking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteProcessRecipe {
    pub command: ProcessTemplate,
    pub args: Vec<ProcessTemplate>,
    pub env: BTreeMap<BString, ProcessTemplate>,
    pub current_dir: ProcessTemplate,
    pub work_dir: RecipeRef,
    pub output_scaffold: Option<RecipeRef>,
    pub platform: Platform,
    pub is_unsafe: bool,
    pub networking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTemplate {
    pub components: Vec<ProcessTemplateComponent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessTemplateComponent {
    Literal { value: BString },
    Input { recipe: RecipeRef },
    OutputPath,
    ResourceDir,
    InputResourceDirs,
    HomeDir,
    WorkDir,
    TempDir,
    CaCertificateBundlePath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeKind {
    File,
    Directory,
    Symlink,
    Download,
    Unarchive,
    Process,
    CompleteProcess,
    CreateFile,
    CreateDirectory,
    Cast,
    Merge,
    Peel,
    Get,
    Insert,
    Glob,
    SetPermissions,
    CollectReferences,
    AttachResources,
    Proxy,
    Sync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Tar,
    Zip,
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
