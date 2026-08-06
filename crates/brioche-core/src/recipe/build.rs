use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use anyhow::Context as _;

use crate::{
    BriocheState,
    blob::BlobHash,
    recipe::{Recipe, RecipeRef, Recipes, Symlink},
};

pub enum ArtifactBuilder {
    File {
        executable: bool,
        content_blob: BlobHash,
        resources: Box<Option<Self>>,
    },
    Symlink {
        target: bstr::BString,
    },
    Directory {
        entries: HashMap<bstr::BString, Option<Self>>,
    },
    Reference {
        source_path: ArtifactPath,
    },
}

impl ArtifactBuilder {
    #[must_use]
    pub fn empty_dir() -> Self {
        Self::Directory {
            entries: HashMap::new(),
        }
    }

    pub fn from_artifact(brioche: &BriocheState, recipe_ref: RecipeRef) -> anyhow::Result<Self> {
        Self::from_artifact_inner(&brioche.recipes, recipe_ref)
    }

    fn from_artifact_inner(recipes: &Recipes, recipe_ref: RecipeRef) -> anyhow::Result<Self> {
        match &**recipes.get_recipe(recipe_ref) {
            Recipe::File(file) => {
                let resources = file
                    .resources
                    .map(|resources| Self::from_artifact_inner(recipes, resources))
                    .transpose()?;
                Ok(Self::File {
                    executable: file.executable,
                    content_blob: file.content_blob,
                    resources: Box::new(resources),
                })
            }
            Recipe::Directory(directory) => {
                let entries = directory
                    .entries
                    .iter()
                    .map(|(entry_name, entry)| {
                        let entry = Self::from_artifact_inner(recipes, *entry)?;
                        anyhow::Ok((entry_name.clone(), Some(entry)))
                    })
                    .collect::<anyhow::Result<_>>()?;
                Ok(Self::Directory { entries })
            }
            Recipe::Symlink(symlink) => Ok(Self::Symlink {
                target: symlink.target.clone(),
            }),
            recipe => anyhow::bail!("recipe is not an artifact: {:?}", recipe.kind()),
        }
    }

    pub fn is_empty_dir(&self) -> bool {
        match self {
            Self::Directory { entries } => entries.values().all(Option::is_none),
            _ => false,
        }
    }
}

/// Build the final `Artifact` from the partial builder tree, resolving
/// `Reference` placeholders against the same tree.
pub fn build_artifact(
    brioche: &mut BriocheState,
    root: &ArtifactBuilder,
) -> anyhow::Result<RecipeRef> {
    build_artifact_inner(&mut brioche.recipes, root)
}

pub(crate) fn build_artifact_inner(
    recipes: &mut Recipes,
    root: &ArtifactBuilder,
) -> anyhow::Result<RecipeRef> {
    // Identity-keyed memo so each unique subtree converts to an `Artifact`
    // exactly once, regardless of how many references resolve to it.
    let mut memo = HashMap::new();
    build_artifact_node(recipes, root, root, &mut memo)
}

fn build_artifact_node(
    recipes: &mut Recipes,
    node: &ArtifactBuilder,
    root: &ArtifactBuilder,
    memo: &mut HashMap<usize, RecipeRef>,
) -> anyhow::Result<RecipeRef> {
    let key = std::ptr::from_ref(node).addr();
    if let Some(cached) = memo.get(&key) {
        return Ok(*cached);
    }

    let recipe_ref = match node {
        ArtifactBuilder::File {
            executable,
            content_blob,
            resources,
        } => {
            let resources = resources
                .as_ref()
                .as_ref()
                .filter(|resources| resources.is_empty_dir());
            let resources = resources
                .map(|resources| build_artifact_node(recipes, resources, root, memo))
                .transpose()?;
            let resources = resources.filter(|resources| {
                let resources = recipes.get_recipe(*resources);
                !resources.is_empty_dir()
            });
            let artifact = Recipe::File(crate::recipe::File {
                content_blob: *content_blob,
                executable: *executable,
                resources,
            });
            recipes.insert_recipe(Arc::new(artifact))
        }
        ArtifactBuilder::Symlink { target } => {
            let artifact = Recipe::Symlink(Symlink {
                target: target.clone(),
            });
            recipes.insert_recipe(Arc::new(artifact))
        }
        ArtifactBuilder::Directory { entries } => {
            let entries = entries
                .iter()
                .filter_map(|(name, entry)| Some((name, entry.as_ref()?)))
                .map(|(name, entry)| {
                    let entry = build_artifact_node(recipes, entry, root, memo)?;
                    Ok((name.clone(), entry))
                })
                .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
            let artifact = Recipe::Directory(crate::recipe::Directory { entries });
            recipes.insert_recipe(Arc::new(artifact))
        }
        ArtifactBuilder::Reference { source_path } => {
            let source_node =
                get_subtree(Some(root), &source_path.components).with_context(|| {
                    format!(
                        "reference source path {:?} not found",
                        source_path.display_pretty()
                    )
                })?;
            build_artifact_node(recipes, source_node, root, memo)?
        }
    };

    memo.insert(key, recipe_ref);
    Ok(recipe_ref)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ArtifactPath {
    pub components: Vec<ArtifactPathComponent>,
}

impl ArtifactPath {
    pub fn new(path: impl AsRef<[u8]>) -> Result<Self, ToArtifactPathError> {
        crate::path::RelativePath::new(path).try_into()
    }

    #[must_use]
    pub fn join_one(&self, component: ArtifactPathComponent) -> Self {
        let mut new = self.clone();
        new.components.push(component);
        new
    }

    #[must_use]
    pub fn join(&self, other: Self) -> Self {
        let mut new = self.clone();
        new.components.extend(other.components);
        new
    }

    #[must_use]
    pub fn display_pretty(&self) -> String {
        let mut display_pretty = String::new();
        for component in &self.components {
            match component {
                ArtifactPathComponent::DirectoryEntry(name) => {
                    display_pretty.push('/');
                    display_pretty.push_str(&urlencoding::encode_binary(name));
                }
                ArtifactPathComponent::FileResources => {
                    display_pretty.push('$');
                }
            }
        }

        display_pretty
    }
}

impl TryFrom<crate::path::RelativePath> for ArtifactPath {
    type Error = ToArtifactPathError;

    fn try_from(value: crate::path::RelativePath) -> Result<Self, Self::Error> {
        let mut components = vec![];
        for component in value.into_components() {
            match component {
                crate::path::RelativePathComponent::CurrentDir => {}
                crate::path::RelativePathComponent::ParentDir => {
                    let popped = components.pop();
                    if popped.is_none() {
                        return Err(ToArtifactPathError::SubpathEscapesTopLevel);
                    }
                }
                crate::path::RelativePathComponent::Normal(bstring) => {
                    components.push(ArtifactPathComponent::DirectoryEntry(bstring));
                }
            }
        }

        Ok(Self { components })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArtifactPathComponent {
    DirectoryEntry(bstr::BString),
    FileResources,
}

impl ArtifactPathComponent {
    pub fn entry(name: impl AsRef<[u8]>) -> Self {
        Self::DirectoryEntry(name.as_ref().into())
    }
}

pub fn insert_into_artifact(
    container: &mut Option<ArtifactBuilder>,
    path: &ArtifactPath,
    artifact: ArtifactBuilder,
) -> anyhow::Result<()> {
    tracing::info!(
        path = path.display_pretty(),
        kind = match artifact {
            ArtifactBuilder::File { .. } => "file",
            ArtifactBuilder::Symlink { .. } => "symlink",
            ArtifactBuilder::Directory { .. } => "directory",
            ArtifactBuilder::Reference { .. } => "reference",
        },
        "inserting into artifact"
    );

    insert_into_artifact_inner(
        container,
        path,
        &path.components,
        artifact,
        InsertOnConflict::Error,
    )?;

    Ok(())
}

pub fn insert_or_replace_in_artifact(
    container: &mut Option<ArtifactBuilder>,
    path: &ArtifactPath,
    artifact: ArtifactBuilder,
) -> anyhow::Result<Option<ArtifactBuilder>> {
    tracing::info!(
        path = path.display_pretty(),
        kind = match artifact {
            ArtifactBuilder::File { .. } => "file",
            ArtifactBuilder::Symlink { .. } => "symlink",
            ArtifactBuilder::Directory { .. } => "directory",
            ArtifactBuilder::Reference { .. } => "reference",
        },
        "inserting into artifact"
    );

    insert_into_artifact_inner(
        container,
        path,
        &path.components,
        artifact,
        InsertOnConflict::Replace,
    )
}

#[derive(Debug, Clone, Copy)]
enum InsertOnConflict {
    Replace,
    Error,
}

fn insert_into_artifact_inner(
    container: &mut Option<ArtifactBuilder>,
    full_path: &ArtifactPath,
    components: &[ArtifactPathComponent],
    artifact: ArtifactBuilder,
    on_conflict: InsertOnConflict,
) -> anyhow::Result<Option<ArtifactBuilder>> {
    let replaced = match components {
        [] => match on_conflict {
            InsertOnConflict::Error => {
                anyhow::ensure!(
                    container.is_none(),
                    "archive entry tried to override path {:?}",
                    full_path.display_pretty()
                );
                *container = Some(artifact);
                None
            }
            InsertOnConflict::Replace => container.replace(artifact),
        },
        [ArtifactPathComponent::DirectoryEntry(name), rest @ ..] => {
            let container = container.get_or_insert_with(ArtifactBuilder::empty_dir);
            let ArtifactBuilder::Directory { entries } = container else {
                anyhow::bail!(
                    "path {:?} descends into non-directory",
                    full_path.display_pretty()
                );
            };
            let entry = entries.entry(name.to_owned()).or_default();
            insert_into_artifact_inner(entry, full_path, rest, artifact, on_conflict)?
        }
        [ArtifactPathComponent::FileResources, rest @ ..] => {
            let Some(container) = container else {
                anyhow::bail!(
                    "path {:?} tried to add resource to a file that doesn't exist",
                    full_path.display_pretty()
                );
            };
            let ArtifactBuilder::File { resources, .. } = container else {
                anyhow::bail!(
                    "path {:?} tried to add resource to a non-file",
                    full_path.display_pretty()
                );
            };

            insert_into_artifact_inner(resources.as_mut(), full_path, rest, artifact, on_conflict)?
        }
    };

    Ok(replaced)
}

/// Get a reference to a subtree at the given path components.
fn get_subtree<'a>(
    container: Option<&'a ArtifactBuilder>,
    components: &[ArtifactPathComponent],
) -> Option<&'a ArtifactBuilder> {
    match components {
        [] => container,
        [ArtifactPathComponent::DirectoryEntry(name), rest @ ..] => {
            let ArtifactBuilder::Directory { entries } = container.as_ref()? else {
                return None;
            };
            let entry = entries.get(name)?;
            get_subtree(entry.as_ref(), rest)
        }
        [ArtifactPathComponent::FileResources, rest @ ..] => {
            let ArtifactBuilder::File { resources, .. } = container.as_ref()? else {
                return None;
            };
            get_subtree(resources.as_ref().as_ref(), rest)
        }
    }
}

/// Set a subtree at the given path components.
pub fn set_subtree(
    container: &mut Option<ArtifactBuilder>,
    components: &[ArtifactPathComponent],
    subtree: ArtifactBuilder,
) -> anyhow::Result<()> {
    match components {
        [] => {
            *container = Some(subtree);
            Ok(())
        }
        [ArtifactPathComponent::DirectoryEntry(name), rest @ ..] => {
            let container = container.get_or_insert_with(ArtifactBuilder::empty_dir);
            let ArtifactBuilder::Directory { entries } = container else {
                anyhow::bail!("tried to descend into non-directory");
            };
            let entry = entries.entry(name.to_owned()).or_default();
            set_subtree(entry, rest, subtree)
        }
        [ArtifactPathComponent::FileResources, rest @ ..] => {
            let Some(container) = container else {
                anyhow::bail!("tried to set resources on non-existent file");
            };
            let ArtifactBuilder::File { resources, .. } = container else {
                anyhow::bail!("tried to set resources on non-file");
            };
            set_subtree(resources.as_mut(), rest, subtree)
        }
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum ToArtifactPathError {
    #[error("subpath escapes top-level path")]
    SubpathEscapesTopLevel,
}
