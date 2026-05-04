use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bstr::BString;
use petgraph::visit::EdgeRef;

use crate::{
    blob::BlobHash,
    encoding::TickEncoded,
    hash::AnyHash,
    platform::Platform,
    recipe::{
        ArchiveFormat, ArtifactKind, CompressionFormat,
        graph::{
            AttachResourcesEdge, AttachResourcesNode, CastEdge, CastNode, CollectReferencesEdge,
            CollectReferencesNode, CreateDirectoryNode, CreateFileNode, DirectoryEdge,
            DirectoryNode, DownloadNode, FileEdge, FileNode, GetEdge, GetNode, GlobEdge, GlobNode,
            InsertEdge, InsertNode, MergeEdge, MergeNode, PeelEdge, PeelNode, ProxyEdge, ProxyNode,
            RecipeEdge, RecipeGraphEdge, RecipeGraphNode, RecipeRef, SetPermissionsEdge,
            SetPermissionsNode, SymlinkNode, SyncEdge, SyncNode, UnarchiveEdge, UnarchiveNode,
        },
    },
};

use super::graph::RecipeNode;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RecipeHash(crate::hash::Blake3Hash);

pub(crate) fn hash_recipes_within(
    recipes: &super::Recipes,
    recipe_refs: HashSet<RecipeRef>,
) -> HashMap<RecipeRef, RecipeHash> {
    // Create a copy of the graph, but keeping only nodes that are reachable
    // from the recipes we're hashing
    let mut graph = recipes.graph.clone();
    let mut dfs_space = petgraph::algo::DfsSpace::default();
    graph.retain_nodes(|graph, index| {
        recipe_refs.contains(&RecipeRef(index))
            || recipe_refs.iter().any(|recipe_ref| {
                petgraph::algo::has_path_connecting(
                    &*graph,
                    recipe_ref.0,
                    index,
                    Some(&mut dfs_space),
                )
            })
    });

    let node_indices = petgraph::algo::toposort(&graph, Some(&mut dfs_space)).unwrap_or_else(|error| {
        panic!("Encountered cycle (which includes {:?}) in recipe graph while trying to hash recipes", error.node_id())
    });

    let mut recipes = HashMap::<RecipeRef, ContentAddressedRecipe>::new();
    let mut recipe_hashes = HashMap::new();

    for node_index in node_indices {
        let node = &graph[node_index];
        let RecipeGraphNode::Recipe(node) = node else {
            continue;
        };
        let recipe_ref = RecipeRef(node_index);

        let recipe = match node {
            RecipeNode::File(FileNode {
                content_blob,
                executable,
            }) => {
                let mut resources = None;
                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::File(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        FileEdge::Resources => {
                            resources = Some(recipes[&RecipeRef(edge.target())].clone());
                        }
                    }
                }

                let resources = resources.unwrap_or_else(|| ContentAddressedRecipe::Directory {
                    entries: BTreeMap::new(),
                });

                ContentAddressedRecipe::File {
                    content_blob: *content_blob,
                    executable: *executable,
                    resources: Box::new(resources),
                }
            }
            RecipeNode::Directory(DirectoryNode {}) => {
                let mut entries = BTreeMap::new();

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Directory(weight)) = edge.weight()
                    else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        DirectoryEdge::Entry { name } => {
                            let target_ref = RecipeRef(edge.target());
                            let target_hash =
                                recipe_hashes.entry(target_ref).or_insert_with(|| {
                                    content_addressed_recipe_hash(&recipes[&target_ref])
                                });
                            entries.insert(name.clone(), *target_hash);
                        }
                    }
                }

                ContentAddressedRecipe::Directory { entries }
            }
            RecipeNode::Symlink(SymlinkNode { target }) => ContentAddressedRecipe::Symlink {
                target: target.clone(),
            },
            RecipeNode::Download(DownloadNode { url, hash }) => ContentAddressedRecipe::Download {
                url: url.clone(),
                hash: hash.clone(),
            },
            RecipeNode::Unarchive(UnarchiveNode {
                archive,
                compression,
            }) => {
                let mut file = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Unarchive(weight)) = edge.weight()
                    else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        UnarchiveEdge::File => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            file = Some(target_recipe);
                        }
                    }
                }

                let file = file.expect("Missing edge for Unarchive recipe");

                ContentAddressedRecipe::Unarchive {
                    file: Box::new(file),
                    archive: *archive,
                    compression: *compression,
                }
            }
            RecipeNode::Process(_) => {
                todo!()
            }
            RecipeNode::CompleteProcess(_) => {
                todo!()
            }
            RecipeNode::CreateFile(CreateFileNode {
                content,
                executable,
            }) => {
                let mut resources = None;
                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::File(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        FileEdge::Resources => {
                            resources = Some(recipes[&RecipeRef(edge.target())].clone());
                        }
                    }
                }

                let resources = resources.unwrap_or_else(|| ContentAddressedRecipe::Directory {
                    entries: BTreeMap::new(),
                });

                ContentAddressedRecipe::CreateFile {
                    content: content.clone(),
                    executable: *executable,
                    resources: Box::new(resources),
                }
            }
            RecipeNode::CreateDirectory(CreateDirectoryNode {}) => {
                let mut entries = BTreeMap::new();

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Directory(weight)) = edge.weight()
                    else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        DirectoryEdge::Entry { name } => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            entries.insert(name.clone(), target_recipe);
                        }
                    }
                }

                ContentAddressedRecipe::CreateDirectory { entries }
            }
            RecipeNode::Cast(CastNode { to }) => {
                let mut recipe = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Cast(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        CastEdge::Recipe => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            recipe = Some(target_recipe);
                        }
                    }
                }

                let recipe = recipe.expect("Missing edge for Cast recipe");

                ContentAddressedRecipe::Cast {
                    recipe: Box::new(recipe),
                    to: *to,
                }
            }
            RecipeNode::Merge(MergeNode {}) => {
                let mut directories = BTreeMap::new();

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Merge(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        MergeEdge::Directory { index } => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            directories.insert(index, target_recipe);
                        }
                    }
                }

                let directories = directories.into_values().collect();

                ContentAddressedRecipe::Merge { directories }
            }
            RecipeNode::Peel(PeelNode { depth }) => {
                let mut directory = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Peel(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        PeelEdge::Directory => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            directory = Some(target_recipe);
                        }
                    }
                }

                let directory = directory.expect("Missing edge for Peel recipe");

                ContentAddressedRecipe::Peel {
                    directory: Box::new(directory),
                    depth: *depth,
                }
            }
            RecipeNode::Get(GetNode { path }) => {
                let mut directory = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Get(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        GetEdge::Directory => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            directory = Some(target_recipe);
                        }
                    }
                }

                let directory = directory.expect("Missing edge for Get recipe");

                ContentAddressedRecipe::Get {
                    directory: Box::new(directory),
                    path: path.clone(),
                }
            }
            RecipeNode::Insert(InsertNode { path }) => {
                let mut directory = None;
                let mut recipe = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Insert(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        InsertEdge::Directory => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            directory = Some(target_recipe);
                        }
                        InsertEdge::Recipe => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            recipe = Some(target_recipe);
                        }
                    }
                }

                let directory = directory.expect("Missing edge for Insert recipe");

                ContentAddressedRecipe::Insert {
                    directory: Box::new(directory),
                    path: path.clone(),
                    recipe: recipe.map(Box::new),
                }
            }
            RecipeNode::Glob(GlobNode { patterns }) => {
                let mut directory = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Glob(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        GlobEdge::Directory => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            directory = Some(target_recipe);
                        }
                    }
                }

                let directory = directory.expect("Missing edge for Glob recipe");

                ContentAddressedRecipe::Glob {
                    directory: Box::new(directory),
                    patterns: patterns.clone(),
                }
            }
            RecipeNode::SetPermissions(SetPermissionsNode { executable }) => {
                let mut file = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::SetPermissions(weight)) = edge.weight()
                    else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        SetPermissionsEdge::File => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            file = Some(target_recipe);
                        }
                    }
                }

                let file = file.expect("Missing edge for SetPermissions recipe");

                ContentAddressedRecipe::SetPermissions {
                    file: Box::new(file),
                    executable: *executable,
                }
            }
            RecipeNode::CollectReferences(CollectReferencesNode {}) => {
                let mut recipe = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::CollectReferences(weight)) =
                        edge.weight()
                    else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        CollectReferencesEdge::Recipe => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            recipe = Some(target_recipe);
                        }
                    }
                }

                let recipe = recipe.expect("Missing edge for CollectReferences recipe");

                ContentAddressedRecipe::CollectReferences {
                    recipe: Box::new(recipe),
                }
            }
            RecipeNode::AttachResources(AttachResourcesNode {}) => {
                let mut recipe = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::AttachResources(weight)) =
                        edge.weight()
                    else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        AttachResourcesEdge::Recipe => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            recipe = Some(target_recipe);
                        }
                    }
                }

                let recipe = recipe.expect("Missing edge for AttachResources recipe");

                ContentAddressedRecipe::AttachResources {
                    recipe: Box::new(recipe),
                }
            }
            RecipeNode::Proxy(ProxyNode {}) => {
                let mut recipe_hash = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Proxy(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        ProxyEdge::Recipe => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe_hash =
                                recipe_hashes.entry(target_ref).or_insert_with(|| {
                                    content_addressed_recipe_hash(&recipes[&target_ref])
                                });
                            recipe_hash = Some(*target_recipe_hash);
                        }
                    }
                }

                let recipe_hash = recipe_hash.expect("Missing edge for Proxy recipe");

                ContentAddressedRecipe::Proxy {
                    recipe: recipe_hash,
                }
            }
            RecipeNode::Sync(SyncNode {}) => {
                let mut recipe = None;

                for edge in graph.edges(node_index) {
                    let RecipeGraphEdge::Recipe(RecipeEdge::Sync(weight)) = edge.weight() else {
                        unreachable!("Invalid edge type");
                    };

                    match weight {
                        SyncEdge::Recipe => {
                            let target_ref = RecipeRef(edge.target());
                            let target_recipe = recipes[&target_ref].clone();
                            recipe = Some(target_recipe);
                        }
                    }
                }

                let recipe = recipe.expect("Missing edge for Sync recipe");

                ContentAddressedRecipe::Sync {
                    recipe: Box::new(recipe),
                }
            }
        };

        recipes.insert(recipe_ref, recipe);
    }

    for recipe_ref in recipe_refs {
        recipe_hashes.entry(recipe_ref).or_insert_with(|| {
            let recipe = &recipes[&recipe_ref];
            content_addressed_recipe_hash(recipe)
        });
    }

    todo!();
}

fn content_addressed_recipe_hash(recipe: &ContentAddressedRecipe) -> RecipeHash {
    let mut hasher = blake3::Hasher::new();
    json_canon::to_writer(&mut hasher, recipe).expect("failed to serialize recipe");
    RecipeHash(hasher.finalize().into())
}

impl std::fmt::Display for RecipeHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ContentAddressedRecipe {
    #[serde(rename_all = "camelCase")]
    File {
        content_blob: BlobHash,
        executable: bool,
        resources: Box<Self>,
    },
    #[serde(rename_all = "camelCase")]
    Directory {
        #[serde_as(as = "BTreeMap<TickEncoded, _>")]
        entries: BTreeMap<BString, RecipeHash>,
    },
    #[serde(rename_all = "camelCase")]
    Symlink {
        #[serde_as(as = "TickEncoded")]
        target: BString,
    },
    #[serde(rename_all = "camelCase")]
    Download {
        url: url::Url,
        hash: AnyHash,
    },
    #[serde(rename_all = "camelCase")]
    Unarchive {
        file: Box<Self>,
        archive: ArchiveFormat,
        #[serde(default)]
        compression: CompressionFormat,
    },
    Process(ContentAddressedProcessRecipe),
    CompleteProcess(ContentAddressedProcessRecipe),
    #[serde(rename_all = "camelCase")]
    CreateFile {
        #[serde_as(as = "TickEncoded")]
        content: BString,
        executable: bool,
        resources: Box<Self>,
    },
    #[serde(rename_all = "camelCase")]
    CreateDirectory {
        entries: BTreeMap<BString, Self>,
    },
    #[serde(rename_all = "camelCase")]
    Cast {
        recipe: Box<Self>,
        to: ArtifactKind,
    },
    #[serde(rename_all = "camelCase")]
    Merge {
        directories: Vec<Self>,
    },
    #[serde(rename_all = "camelCase")]
    Peel {
        directory: Box<Self>,
        depth: u32,
    },
    #[serde(rename_all = "camelCase")]
    Get {
        directory: Box<Self>,
        #[serde_as(as = "TickEncoded")]
        path: BString,
    },
    #[serde(rename_all = "camelCase")]
    Insert {
        directory: Box<Self>,
        #[serde_as(as = "TickEncoded")]
        path: BString,
        recipe: Option<Box<Self>>,
    },
    Glob {
        directory: Box<Self>,
        patterns: BTreeSet<BString>,
    },
    #[serde(rename_all = "camelCase")]
    SetPermissions {
        file: Box<Self>,
        executable: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    CollectReferences {
        recipe: Box<Self>,
    },
    #[serde(rename_all = "camelCase")]
    AttachResources {
        recipe: Box<Self>,
    },
    #[serde(rename_all = "camelCase")]
    Proxy {
        recipe: RecipeHash,
    },
    #[serde(rename_all = "camelCase")]
    Sync {
        recipe: Box<Self>,
    },
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentAddressedProcessRecipe {
    pub command: ContentAddressedProcessTemplate,

    pub args: Vec<ContentAddressedProcessTemplate>,

    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    pub env: BTreeMap<BString, ContentAddressedProcessTemplate>,

    #[serde(
        default = "ContentAddressedProcessTemplate::default_current_dir",
        skip_serializing_if = "ContentAddressedProcessTemplate::is_default_current_dir"
    )]
    pub current_dir: ContentAddressedProcessTemplate,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ContentAddressedRecipe>,

    pub work_dir: Box<ContentAddressedRecipe>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_scaffold: Option<Box<ContentAddressedRecipe>>,

    pub platform: Platform,

    #[serde(
        rename = "unsafe",
        default,
        skip_serializing_if = "crate::utils::is_default"
    )]
    pub is_unsafe: bool,

    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub networking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentAddressedProcessTemplate {
    pub components: Vec<ContentAddressedProcessTemplateComponent>,
}

impl ContentAddressedProcessTemplate {
    #[must_use]
    pub fn default_current_dir() -> Self {
        Self {
            components: vec![ContentAddressedProcessTemplateComponent::WorkDir],
        }
    }

    fn is_default_current_dir(&self) -> bool {
        let Self { components } = self;
        components == &[ContentAddressedProcessTemplateComponent::WorkDir]
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ContentAddressedProcessTemplateComponent {
    Literal {
        #[serde_as(as = "TickEncoded")]
        value: BString,
    },
    Input(ContentAddressedProcessTemplateInputComponent),
    OutputPath,
    ResourceDir,
    InputResourceDirs,
    HomeDir,
    WorkDir,
    TempDir,
    CaCertificateBundlePath,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ContentAddressedProcessTemplateInputComponent {
    Recipe { recipe: ContentAddressedRecipe },
    Artifact { artifact: ContentAddressedRecipe },
}

#[derive(Debug, thiserror::Error)]
enum DirectoryFromArtifactError {
    #[error("expected directory artifact")]
    ExpectedDirectoryArtifact,
}
