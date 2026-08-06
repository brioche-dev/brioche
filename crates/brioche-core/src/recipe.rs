use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use bstr::BString;

use crate::{
    BriocheState,
    blob::BlobHash,
    hash::AnyHash,
    platform::Platform,
    recipe::graph::{RecipeGraphEdge, RecipeGraphNode},
};

pub mod build;
mod graph;
pub mod hash;
pub mod load;

pub use graph::RecipeRef;
pub use hash::RecipeHash;

#[must_use]
pub fn get_recipe(brioche: &BriocheState, recipe_ref: RecipeRef) -> Arc<Recipe> {
    brioche.recipes.get_recipe(recipe_ref).clone()
}

pub fn insert_recipe(brioche: &mut BriocheState, recipe: Arc<Recipe>) -> RecipeRef {
    brioche.recipes.insert_recipe(recipe)
}

#[derive(Default)]
pub struct Recipes {
    graph: graph::RecipeGraph,
    recipes: HashMap<RecipeRef, Arc<Recipe>>,
    recipe_refs_by_recipe: HashMap<Arc<Recipe>, RecipeRef>,
    content_addressed_recipes: HashMap<RecipeRef, Arc<hash::ContentAddressedRecipe>>,
    recipe_hashes: HashMap<RecipeRef, RecipeHash>,
}

impl Recipes {
    #[must_use]
    pub fn get_recipe(&self, recipe_ref: RecipeRef) -> &Arc<Recipe> {
        &self.recipes[&recipe_ref]
    }

    pub fn insert_recipe(&mut self, recipe: Arc<Recipe>) -> RecipeRef {
        let recipe_ref = match self.recipe_refs_by_recipe.entry(recipe.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                return *entry.get();
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let recipe_ref = RecipeRef(self.graph.add_node(RecipeGraphNode));
                *entry.insert(recipe_ref)
            }
        };

        let mut edge_refs = vec![];
        recipe.push_recipe_refs(&mut edge_refs);

        for edge_ref in edge_refs {
            self.graph
                .update_edge(recipe_ref.0, edge_ref.0, RecipeGraphEdge);
        }

        self.recipes.insert(recipe_ref, recipe);

        recipe_ref
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Artifact {
    File(File),
    Directory(Directory),
    Symlink(Symlink),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct File {
    pub content_blob: BlobHash,
    pub executable: bool,
    pub resources: Option<RecipeRef>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct Directory {
    pub entries: BTreeMap<BString, RecipeRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symlink {
    pub target: BString,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

impl Recipe {
    #[must_use]
    pub const fn kind(&self) -> RecipeKind {
        match self {
            Self::File(..) => RecipeKind::File,
            Self::Directory(..) => RecipeKind::Directory,
            Self::Symlink(..) => RecipeKind::Symlink,
            Self::Download(..) => RecipeKind::Download,
            Self::Unarchive(..) => RecipeKind::Unarchive,
            Self::Process(..) => RecipeKind::Process,
            Self::CompleteProcess(..) => RecipeKind::CompleteProcess,
            Self::CreateFile { .. } => RecipeKind::CreateFile,
            Self::CreateDirectory { .. } => RecipeKind::CreateDirectory,
            Self::Cast { .. } => RecipeKind::Cast,
            Self::Merge { .. } => RecipeKind::Merge,
            Self::Peel { .. } => RecipeKind::Peel,
            Self::Get { .. } => RecipeKind::Get,
            Self::Insert { .. } => RecipeKind::Insert,
            Self::Glob { .. } => RecipeKind::Glob,
            Self::SetPermissions { .. } => RecipeKind::SetPermissions,
            Self::CollectReferences { .. } => RecipeKind::CollectReferences,
            Self::AttachResources { .. } => RecipeKind::AttachResources,
            Self::Proxy { .. } => RecipeKind::Proxy,
            Self::Sync { .. } => RecipeKind::Sync,
        }
    }

    #[must_use]
    pub fn is_empty_dir(&self) -> bool {
        match self {
            Self::Directory(directory) => directory.entries.is_empty(),
            _ => false,
        }
    }

    fn push_recipe_refs(&self, recipe_refs: &mut Vec<RecipeRef>) {
        match self {
            Self::File(file) => {
                recipe_refs.extend(file.resources);
            }
            Self::Directory(directory) => {
                recipe_refs.extend(directory.entries.values().copied());
            }
            Self::Symlink(_) | Self::Download(_) => {}
            Self::Unarchive(unarchive) => {
                recipe_refs.push(unarchive.file);
            }
            Self::Process(process) => {
                process.push_recipe_refs(recipe_refs);
            }
            Self::CompleteProcess(complete_process) => {
                complete_process.push_recipe_refs(recipe_refs);
            }
            Self::CreateFile {
                content: _,
                executable: _,
                resources,
            } => {
                recipe_refs.extend(resources);
            }
            Self::CreateDirectory { entries } => {
                recipe_refs.extend(entries.values().copied());
            }
            Self::Cast { recipe, to: _ }
            | Self::CollectReferences { recipe }
            | Self::AttachResources { recipe }
            | Self::Proxy { recipe }
            | Self::Sync { recipe } => {
                recipe_refs.push(*recipe);
            }
            Self::Merge { directories } => {
                recipe_refs.extend_from_slice(directories);
            }
            Self::Peel {
                directory,
                depth: _,
            }
            | Self::Get { directory, path: _ }
            | Self::Glob {
                directory,
                patterns: _,
            } => {
                recipe_refs.push(*directory);
            }
            Self::Insert {
                directory,
                path: _,
                recipe,
            } => {
                recipe_refs.push(*directory);
                recipe_refs.extend(*recipe);
            }
            Self::SetPermissions {
                file,
                executable: _,
            } => {
                recipe_refs.push(*file);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DownloadRecipe {
    pub url: url::Url,
    pub hash: AnyHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnarchiveRecipe {
    pub file: RecipeRef,
    pub archive: ArchiveFormat,
    pub compression: CompressionFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

impl ProcessRecipe {
    fn push_recipe_refs(&self, recipe_refs: &mut Vec<RecipeRef>) {
        let Self {
            command,
            args,
            env,
            current_dir,
            dependencies,
            work_dir,
            output_scaffold,
            platform: _,
            is_unsafe: _,
            networking: _,
        } = self;
        command.push_recipe_refs(recipe_refs);
        for arg in args {
            arg.push_recipe_refs(recipe_refs);
        }
        for env_value in env.values() {
            env_value.push_recipe_refs(recipe_refs);
        }
        current_dir.push_recipe_refs(recipe_refs);
        recipe_refs.extend_from_slice(dependencies);
        recipe_refs.push(*work_dir);
        recipe_refs.extend(*output_scaffold);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

impl CompleteProcessRecipe {
    fn push_recipe_refs(&self, recipe_refs: &mut Vec<RecipeRef>) {
        let Self {
            command,
            args,
            env,
            current_dir,
            work_dir,
            output_scaffold,
            platform: _,
            is_unsafe: _,
            networking: _,
        } = self;
        command.push_recipe_refs(recipe_refs);
        for arg in args {
            arg.push_recipe_refs(recipe_refs);
        }
        for env_value in env.values() {
            env_value.push_recipe_refs(recipe_refs);
        }
        current_dir.push_recipe_refs(recipe_refs);
        recipe_refs.push(*work_dir);
        recipe_refs.extend(*output_scaffold);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessTemplate {
    pub components: Vec<ProcessTemplateComponent>,
}

impl ProcessTemplate {
    fn push_recipe_refs(&self, recipe_refs: &mut Vec<RecipeRef>) {
        for component in &self.components {
            component.push_recipe_refs(recipe_refs);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

impl ProcessTemplateComponent {
    fn push_recipe_refs(&self, recipe_refs: &mut Vec<RecipeRef>) {
        match self {
            Self::Input { recipe } => {
                recipe_refs.push(*recipe);
            }
            Self::Literal { value: _ }
            | Self::OutputPath
            | Self::ResourceDir
            | Self::InputResourceDirs
            | Self::HomeDir
            | Self::WorkDir
            | Self::TempDir
            | Self::CaCertificateBundlePath => {}
        }
    }
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

#[expect(clippy::unused_async)]
pub async fn commit_recipes(_brioche: &crate::BriocheResources) -> anyhow::Result<()> {
    // TODO: Persist recipes!!
    Ok(())
}
