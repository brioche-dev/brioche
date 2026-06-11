use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use bstr::BString;

use crate::{
    Brioche,
    blob::BlobHash,
    encoding::TickEncoded,
    hash::AnyHash,
    platform::Platform,
    recipe::{ArchiveFormat, ArtifactKind, CompressionFormat, Recipe, RecipeRef},
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RecipeHash(crate::hash::Blake3Hash);

pub async fn hash_recipe(brioche: &Brioche, recipe_ref: RecipeRef) -> RecipeHash {
    let mut recipes = brioche.recipes.write().await;
    let hashes = hash_recipes_within(&mut recipes, [recipe_ref]);
    hashes[&recipe_ref]
}

pub async fn hash_recipes(
    brioche: &Brioche,
    recipe_refs: impl IntoIterator<Item = RecipeRef>,
) -> HashMap<RecipeRef, RecipeHash> {
    let mut recipes = brioche.recipes.write().await;
    hash_recipes_within(&mut recipes, recipe_refs)
}

pub fn hash_recipes_within(
    recipes: &mut super::Recipes,
    recipe_refs: impl IntoIterator<Item = RecipeRef>,
) -> HashMap<RecipeRef, RecipeHash> {
    let empty_dir = std::sync::LazyLock::new(|| {
        Arc::new(ContentAddressedRecipe::Directory {
            entries: BTreeMap::new(),
        })
    });

    let mut dfs = petgraph::visit::DfsPostOrder::empty(&recipes.graph);

    let mut result_recipe_hashes = HashMap::new();
    let mut need_recipe_hashes = HashSet::new();

    for recipe_ref in recipe_refs {
        let recipe_hash_entry = recipes.recipe_hashes.entry(recipe_ref);
        match recipe_hash_entry {
            std::collections::hash_map::Entry::Occupied(entry) => {
                result_recipe_hashes.insert(recipe_ref, *entry.get());
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                if let Some(content_addressed_recipe) =
                    recipes.content_addressed_recipes.get(&recipe_ref)
                {
                    let recipe_hash = content_addressed_recipe_hash(content_addressed_recipe);
                    entry.insert(recipe_hash);

                    result_recipe_hashes.insert(recipe_ref, recipe_hash);
                } else if need_recipe_hashes.insert(recipe_ref) {
                    dfs.stack.push(recipe_ref.0);
                }
            }
        }
    }

    while let Some(node_index) = dfs.next(&recipes.graph) {
        let recipe_ref = RecipeRef(node_index);
        let recipe = &recipes.recipes[&recipe_ref];
        if recipes.content_addressed_recipes.contains_key(&recipe_ref) {
            continue;
        }

        let content_addressed_recipe = match &**recipe {
            Recipe::File(crate::recipe::File {
                content_blob,
                executable,
                resources,
            }) => {
                let resources = resources.map_or_else(
                    || empty_dir.clone(),
                    |resources| recipes.content_addressed_recipes[&resources].clone(),
                );
                Arc::new(ContentAddressedRecipe::File {
                    content_blob: *content_blob,
                    executable: *executable,
                    resources,
                })
            }
            Recipe::Directory(crate::recipe::Directory { entries }) => {
                if entries.is_empty() {
                    empty_dir.clone()
                } else {
                    let entries = entries
                        .iter()
                        .map(|(name, entry)| {
                            let recipe_hash =
                                recipes.recipe_hashes.entry(*entry).or_insert_with(|| {
                                    content_addressed_recipe_hash(
                                        &recipes.content_addressed_recipes[entry],
                                    )
                                });
                            (name.clone(), *recipe_hash)
                        })
                        .collect();
                    Arc::new(ContentAddressedRecipe::Directory { entries })
                }
            }
            Recipe::Symlink(crate::recipe::Symlink { target }) => {
                Arc::new(ContentAddressedRecipe::Symlink {
                    target: target.clone(),
                })
            }
            Recipe::Download(crate::recipe::DownloadRecipe { url, hash }) => {
                Arc::new(ContentAddressedRecipe::Download {
                    url: url.clone(),
                    hash: hash.clone(),
                })
            }
            Recipe::Unarchive(crate::recipe::UnarchiveRecipe {
                archive,
                compression,
                file,
            }) => {
                let file = recipes.content_addressed_recipes[file].clone();
                Arc::new(ContentAddressedRecipe::Unarchive {
                    file,
                    archive: *archive,
                    compression: *compression,
                })
            }
            Recipe::Process(process) => {
                let complete = false;
                let crate::recipe::ProcessRecipe {
                    command,
                    args,
                    env,
                    current_dir,
                    dependencies,
                    work_dir,
                    output_scaffold,
                    platform,
                    is_unsafe,
                    networking,
                } = process;
                Arc::new(ContentAddressedRecipe::Process(
                    ContentAddressedProcessRecipe {
                        command: build_process_template(
                            command,
                            &recipes.content_addressed_recipes,
                            complete,
                        ),
                        args: args
                            .iter()
                            .map(|arg| {
                                build_process_template(
                                    arg,
                                    &recipes.content_addressed_recipes,
                                    complete,
                                )
                            })
                            .collect(),
                        env: env
                            .iter()
                            .map(|(key, value)| {
                                (
                                    key.clone(),
                                    build_process_template(
                                        value,
                                        &recipes.content_addressed_recipes,
                                        complete,
                                    ),
                                )
                            })
                            .collect(),
                        current_dir: build_process_template(
                            current_dir,
                            &recipes.content_addressed_recipes,
                            complete,
                        ),
                        dependencies: dependencies
                            .iter()
                            .map(|dependency| recipes.content_addressed_recipes[dependency].clone())
                            .collect(),
                        work_dir: recipes.content_addressed_recipes[work_dir].clone(),
                        output_scaffold: output_scaffold.map(|output_scaffold| {
                            recipes.content_addressed_recipes[&output_scaffold].clone()
                        }),
                        platform: *platform,
                        is_unsafe: *is_unsafe,
                        networking: *networking,
                    },
                ))
            }
            Recipe::CompleteProcess(complete_process) => {
                let complete = true;
                let crate::recipe::CompleteProcessRecipe {
                    command,
                    args,
                    env,
                    current_dir,
                    work_dir,
                    output_scaffold,
                    platform,
                    is_unsafe,
                    networking,
                } = complete_process;
                Arc::new(ContentAddressedRecipe::CompleteProcess(
                    ContentAddressedProcessRecipe {
                        command: build_process_template(
                            command,
                            &recipes.content_addressed_recipes,
                            complete,
                        ),
                        args: args
                            .iter()
                            .map(|arg| {
                                build_process_template(
                                    arg,
                                    &recipes.content_addressed_recipes,
                                    complete,
                                )
                            })
                            .collect(),
                        env: env
                            .iter()
                            .map(|(key, value)| {
                                (
                                    key.clone(),
                                    build_process_template(
                                        value,
                                        &recipes.content_addressed_recipes,
                                        complete,
                                    ),
                                )
                            })
                            .collect(),
                        current_dir: build_process_template(
                            current_dir,
                            &recipes.content_addressed_recipes,
                            complete,
                        ),
                        dependencies: vec![],
                        work_dir: recipes.content_addressed_recipes[work_dir].clone(),
                        output_scaffold: output_scaffold.map(|output_scaffold| {
                            recipes.content_addressed_recipes[&output_scaffold].clone()
                        }),
                        platform: *platform,
                        is_unsafe: *is_unsafe,
                        networking: *networking,
                    },
                ))
            }
            Recipe::CreateFile {
                content,
                executable,
                resources,
            } => {
                let resources = resources.map_or_else(
                    || empty_dir.clone(),
                    |resources| recipes.content_addressed_recipes[&resources].clone(),
                );
                Arc::new(ContentAddressedRecipe::CreateFile {
                    content: content.clone(),
                    executable: *executable,
                    resources,
                })
            }
            Recipe::CreateDirectory { entries } => {
                let entries = entries
                    .iter()
                    .map(|(name, entry)| {
                        let entry = recipes.content_addressed_recipes[entry].clone();
                        (name.clone(), entry)
                    })
                    .collect();
                Arc::new(ContentAddressedRecipe::CreateDirectory { entries })
            }
            Recipe::Cast { recipe, to } => Arc::new(ContentAddressedRecipe::Cast {
                recipe: recipes.content_addressed_recipes[recipe].clone(),
                to: *to,
            }),
            Recipe::Merge { directories } => {
                let directories = directories
                    .iter()
                    .map(|directory| recipes.content_addressed_recipes[directory].clone())
                    .collect();
                Arc::new(ContentAddressedRecipe::Merge { directories })
            }
            Recipe::Peel { directory, depth } => Arc::new(ContentAddressedRecipe::Peel {
                directory: recipes.content_addressed_recipes[directory].clone(),
                depth: *depth,
            }),
            Recipe::Get { directory, path } => Arc::new(ContentAddressedRecipe::Get {
                directory: recipes.content_addressed_recipes[directory].clone(),
                path: path.clone(),
            }),
            Recipe::Insert {
                directory,
                path,
                recipe,
            } => Arc::new(ContentAddressedRecipe::Insert {
                directory: recipes.content_addressed_recipes[directory].clone(),
                path: path.clone(),
                recipe: recipe.map(|recipe| recipes.content_addressed_recipes[&recipe].clone()),
            }),
            Recipe::Glob {
                directory,
                patterns,
            } => Arc::new(ContentAddressedRecipe::Glob {
                directory: recipes.content_addressed_recipes[directory].clone(),
                patterns: patterns.clone(),
            }),
            Recipe::SetPermissions { file, executable } => {
                Arc::new(ContentAddressedRecipe::SetPermissions {
                    file: recipes.content_addressed_recipes[file].clone(),
                    executable: *executable,
                })
            }
            Recipe::CollectReferences { recipe } => {
                Arc::new(ContentAddressedRecipe::CollectReferences {
                    recipe: recipes.content_addressed_recipes[recipe].clone(),
                })
            }
            Recipe::AttachResources { recipe } => {
                Arc::new(ContentAddressedRecipe::AttachResources {
                    recipe: recipes.content_addressed_recipes[recipe].clone(),
                })
            }
            Recipe::Proxy { recipe } => {
                let recipe_hash = *recipes.recipe_hashes.entry(*recipe).or_insert_with(|| {
                    content_addressed_recipe_hash(&recipes.content_addressed_recipes[recipe])
                });
                Arc::new(ContentAddressedRecipe::Proxy {
                    recipe: recipe_hash,
                })
            }
            Recipe::Sync { recipe } => Arc::new(ContentAddressedRecipe::Sync {
                recipe: recipes.content_addressed_recipes[recipe].clone(),
            }),
        };

        recipes
            .content_addressed_recipes
            .insert(recipe_ref, content_addressed_recipe);
    }

    for recipe_ref in need_recipe_hashes {
        let recipe_hash = *recipes.recipe_hashes.entry(recipe_ref).or_insert_with(|| {
            content_addressed_recipe_hash(&recipes.content_addressed_recipes[&recipe_ref])
        });
        result_recipe_hashes.insert(recipe_ref, recipe_hash);
    }

    result_recipe_hashes
}

fn build_process_template(
    process_template: &crate::recipe::ProcessTemplate,
    content_addressed_recipes: &HashMap<RecipeRef, Arc<ContentAddressedRecipe>>,
    complete: bool,
) -> ContentAddressedProcessTemplate {
    let components = process_template
        .components
        .iter()
        .map(|component| match component {
            crate::recipe::ProcessTemplateComponent::Literal { value } => {
                ContentAddressedProcessTemplateComponent::Literal {
                    value: value.clone(),
                }
            }
            crate::recipe::ProcessTemplateComponent::Input { recipe } => {
                let recipe = content_addressed_recipes[recipe].clone();
                let input = if complete {
                    ContentAddressedProcessTemplateInputComponent::Artifact { artifact: recipe }
                } else {
                    ContentAddressedProcessTemplateInputComponent::Recipe { recipe }
                };
                ContentAddressedProcessTemplateComponent::Input(input)
            }
            crate::recipe::ProcessTemplateComponent::OutputPath => {
                ContentAddressedProcessTemplateComponent::OutputPath
            }
            crate::recipe::ProcessTemplateComponent::ResourceDir => {
                ContentAddressedProcessTemplateComponent::ResourceDir
            }
            crate::recipe::ProcessTemplateComponent::InputResourceDirs => {
                ContentAddressedProcessTemplateComponent::InputResourceDirs
            }
            crate::recipe::ProcessTemplateComponent::HomeDir => {
                ContentAddressedProcessTemplateComponent::HomeDir
            }
            crate::recipe::ProcessTemplateComponent::WorkDir => {
                ContentAddressedProcessTemplateComponent::WorkDir
            }
            crate::recipe::ProcessTemplateComponent::TempDir => {
                ContentAddressedProcessTemplateComponent::TempDir
            }
            crate::recipe::ProcessTemplateComponent::CaCertificateBundlePath => {
                ContentAddressedProcessTemplateComponent::CaCertificateBundlePath
            }
        })
        .collect();

    ContentAddressedProcessTemplate { components }
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
pub(super) enum ContentAddressedRecipe {
    #[serde(rename_all = "camelCase")]
    File {
        content_blob: BlobHash,
        executable: bool,
        resources: Arc<Self>,
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
        file: Arc<Self>,
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
        resources: Arc<Self>,
    },
    #[serde(rename_all = "camelCase")]
    CreateDirectory {
        entries: BTreeMap<BString, Arc<Self>>,
    },
    #[serde(rename_all = "camelCase")]
    Cast {
        recipe: Arc<Self>,
        to: ArtifactKind,
    },
    #[serde(rename_all = "camelCase")]
    Merge {
        directories: Vec<Arc<Self>>,
    },
    #[serde(rename_all = "camelCase")]
    Peel {
        directory: Arc<Self>,
        depth: u32,
    },
    #[serde(rename_all = "camelCase")]
    Get {
        directory: Arc<Self>,
        #[serde_as(as = "TickEncoded")]
        path: BString,
    },
    #[serde(rename_all = "camelCase")]
    Insert {
        directory: Arc<Self>,
        #[serde_as(as = "TickEncoded")]
        path: BString,
        recipe: Option<Arc<Self>>,
    },
    Glob {
        directory: Arc<Self>,
        patterns: BTreeSet<BString>,
    },
    #[serde(rename_all = "camelCase")]
    SetPermissions {
        file: Arc<Self>,
        executable: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    CollectReferences {
        recipe: Arc<Self>,
    },
    #[serde(rename_all = "camelCase")]
    AttachResources {
        recipe: Arc<Self>,
    },
    #[serde(rename_all = "camelCase")]
    Proxy {
        recipe: RecipeHash,
    },
    #[serde(rename_all = "camelCase")]
    Sync {
        recipe: Arc<Self>,
    },
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ContentAddressedProcessRecipe {
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
    pub dependencies: Vec<Arc<ContentAddressedRecipe>>,

    pub work_dir: Arc<ContentAddressedRecipe>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_scaffold: Option<Arc<ContentAddressedRecipe>>,

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
pub(super) struct ContentAddressedProcessTemplate {
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
pub(super) enum ContentAddressedProcessTemplateComponent {
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
pub(super) enum ContentAddressedProcessTemplateInputComponent {
    Recipe {
        recipe: Arc<ContentAddressedRecipe>,
    },
    Artifact {
        artifact: Arc<ContentAddressedRecipe>,
    },
}
