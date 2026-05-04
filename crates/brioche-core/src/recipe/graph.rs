use std::collections::BTreeSet;

use bstr::BString;
use petgraph::graph::NodeIndex;

use crate::{
    blob::BlobHash,
    hash::AnyHash,
    recipe::{ArchiveFormat, ArtifactKind, CompressionFormat},
};

pub(crate) type RecipeGraph =
    petgraph::stable_graph::StableDiGraph<RecipeGraphNode, RecipeGraphEdge>;

#[derive(Debug, Clone)]
pub(crate) enum RecipeGraphNode {
    Recipe(RecipeNode),
    ProcessTemplate(ProcessTemplateNode),
    ProcessTemplateComponent(ProcessTemplateComponentNode),
}

#[derive(Debug, Clone)]
pub(crate) enum RecipeGraphEdge {
    Recipe(RecipeEdge),
    ProcessTemplate(ProcessTemplateEdge),
    ProcessTemplateComponent(ProcessTemplateComponentEdge),
}

#[derive(Debug, Clone)]
pub(crate) enum RecipeNode {
    File(FileNode),
    Directory(DirectoryNode),
    Symlink(SymlinkNode),
    Download(DownloadNode),
    Unarchive(UnarchiveNode),
    Process(ProcessNode),
    CompleteProcess(CompleteProcessNode),
    CreateFile(CreateFileNode),
    CreateDirectory(CreateDirectoryNode),
    Cast(CastNode),
    Merge(MergeNode),
    Peel(PeelNode),
    Get(GetNode),
    Insert(InsertNode),
    Glob(GlobNode),
    SetPermissions(SetPermissionsNode),
    CollectReferences(CollectReferencesNode),
    AttachResources(AttachResourcesNode),
    Proxy(ProxyNode),
    Sync(SyncNode),
}

#[derive(Debug, Clone)]
pub(crate) enum RecipeEdge {
    File(FileEdge),
    Directory(DirectoryEdge),
    Symlink(SymlinkEdge),
    Download(DownloadEdge),
    Unarchive(UnarchiveEdge),
    Process(ProcessEdge),
    CompleteProcess(CompleteProcessEdge),
    CreateFile(CreateFileEdge),
    CreateDirectory(CreateDirectoryEdge),
    Cast(CastEdge),
    Merge(MergeEdge),
    Peel(PeelEdge),
    Get(GetEdge),
    Insert(InsertEdge),
    Glob(GlobEdge),
    SetPermissions(SetPermissionsEdge),
    CollectReferences(CollectReferencesEdge),
    AttachResources(AttachResourcesEdge),
    Proxy(ProxyEdge),
    Sync(SyncEdge),
}

#[derive(Debug, Clone)]
pub struct FileNode {
    pub content_blob: BlobHash,
    pub executable: bool,
}

#[derive(Debug, Clone)]
pub enum FileEdge {
    Resources,
}

#[derive(Debug, Clone)]
pub struct DirectoryNode {}

#[derive(Debug, Clone)]
pub enum DirectoryEdge {
    Entry { name: BString },
}

#[derive(Debug, Clone)]
pub struct SymlinkNode {
    pub target: BString,
}

#[derive(Debug, Clone)]
pub enum SymlinkEdge {}

#[derive(Debug, Clone)]
pub struct DownloadNode {
    pub url: url::Url,
    pub hash: AnyHash,
}

#[derive(Debug, Clone)]
pub enum DownloadEdge {}

#[derive(Debug, Clone)]
pub struct UnarchiveNode {
    pub archive: ArchiveFormat,
    pub compression: CompressionFormat,
}

#[derive(Debug, Clone)]
pub enum UnarchiveEdge {
    File,
}

#[derive(Debug, Clone)]
pub struct ProcessNode {}

#[derive(Debug, Clone)]
pub enum ProcessEdge {}

#[derive(Debug, Clone)]
pub struct CompleteProcessNode {}

#[derive(Debug, Clone)]
pub enum CompleteProcessEdge {}

#[derive(Debug, Clone)]
pub struct CreateFileNode {
    pub content: BString,
    pub executable: bool,
}

#[derive(Debug, Clone)]
pub enum CreateFileEdge {
    Resources,
}

#[derive(Debug, Clone)]
pub struct CreateDirectoryNode {}

#[derive(Debug, Clone)]
pub enum CreateDirectoryEdge {
    Entry { name: BString },
}

#[derive(Debug, Clone)]
pub struct CastNode {
    pub to: ArtifactKind,
}

#[derive(Debug, Clone)]
pub enum CastEdge {
    Recipe,
}

#[derive(Debug, Clone)]
pub struct MergeNode {}

#[derive(Debug, Clone)]
pub enum MergeEdge {
    Directory { index: usize },
}

#[derive(Debug, Clone)]
pub struct PeelNode {
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub enum PeelEdge {
    Directory,
}

#[derive(Debug, Clone)]
pub struct GetNode {
    pub path: BString,
}

#[derive(Debug, Clone)]
pub enum GetEdge {
    Directory,
}

#[derive(Debug, Clone)]
pub struct InsertNode {
    pub path: BString,
}

#[derive(Debug, Clone)]
pub enum InsertEdge {
    Directory,
    Recipe,
}

#[derive(Debug, Clone)]
pub struct GlobNode {
    pub patterns: BTreeSet<BString>,
}

#[derive(Debug, Clone)]
pub enum GlobEdge {
    Directory,
}

#[derive(Debug, Clone)]
pub struct SetPermissionsNode {
    pub executable: Option<bool>,
}

#[derive(Debug, Clone)]
pub enum SetPermissionsEdge {
    File,
}

#[derive(Debug, Clone)]
pub struct CollectReferencesNode {}

#[derive(Debug, Clone)]
pub enum CollectReferencesEdge {
    Recipe,
}

#[derive(Debug, Clone)]
pub struct AttachResourcesNode {}

#[derive(Debug, Clone)]
pub enum AttachResourcesEdge {
    Recipe,
}

#[derive(Debug, Clone)]
pub struct ProxyNode {}

#[derive(Debug, Clone)]
pub enum ProxyEdge {
    Recipe,
}

#[derive(Debug, Clone)]
pub struct SyncNode {}

#[derive(Debug, Clone)]
pub enum SyncEdge {
    Recipe,
}

#[derive(Debug, Clone)]
pub struct ProcessTemplateNode {}

#[derive(Debug, Clone)]
pub enum ProcessTemplateEdge {
    Component { index: usize },
}

#[derive(Debug, Clone)]
pub enum ProcessTemplateComponentNode {
    Literal { value: BString },
    Input,
    OutputPath,
    ResourceDir,
    InputResourceDirs,
    HomeDir,
    WorkDir,
    TempDir,
    CaCertificateBundlePath,
}

#[derive(Debug, Clone)]
pub enum ProcessTemplateComponentEdge {
    InputRecipe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecipeRef(pub(super) NodeIndex);
