use std::collections::{BTreeMap, HashMap, HashSet};

use bstr::ByteSlice as _;
use joinery::JoinableIterator as _;
use petgraph::visit::EdgeRef as _;

use crate::{
    BriocheState,
    encoding::TickEncoded,
    path::{RelativePath, RelativePathComponent},
    project::{ProjectDefinition, ProjectEdge, ProjectRef, StaticRef, WorkspaceRef},
    recipe::build::{ArtifactBuilder, ArtifactPath},
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ProjectHash(crate::hash::Blake3Hash);

impl std::fmt::Display for ProjectHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ProjectHash {
    type Err = crate::hash::ParseHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hash = s.parse()?;
        Ok(Self(hash))
    }
}

pub async fn hash_project(
    brioche: &mut BriocheState,
    project_ref: ProjectRef,
) -> Result<ProjectHash, std::convert::Infallible> {
    let node_groups = group_project_nodes(&brioche.projects, [project_ref]);

    let mut project_hashes = HashMap::<ProjectRef, ProjectHash>::new();

    let mut permit = crate::blob::get_save_blob_permit()
        .await
        .expect("todo: failed to get save blob permit");
    hash_projects_inner(
        brioche,
        &mut permit,
        &node_groups,
        &mut project_hashes,
        None,
    );

    Ok(project_hashes[&project_ref])
}

pub async fn get_content_addressed_project_entries(
    brioche: &mut BriocheState,
    project_ref: ProjectRef,
) -> HashMap<ProjectRef, ContentAddressedProjectEntry> {
    let node_groups = group_project_nodes(&brioche.projects, [project_ref]);

    let mut project_hashes = HashMap::new();
    let mut project_entries = HashMap::new();

    let mut permit = crate::blob::get_save_blob_permit()
        .await
        .expect("todo: failed to get save blob permit");
    hash_projects_inner(
        brioche,
        &mut permit,
        &node_groups,
        &mut project_hashes,
        Some(&mut project_entries),
    );

    project_entries
}

/// Return all transitive dependencies of the projects in `project_refs`
/// (including themselves), grouped into sets of cyclic projects.
///
/// In the project graph, this trims the graph to nodes reachable from
/// any `project_refs` nodes, then finds the [strongly-connected components](https://en.wikipedia.org/wiki/Strongly_connected_component)
/// of the graph. For each group, if there is only one project, it's not part
/// of a cycle; if there's more than one, then the projects all reference
/// each other cyclically. The groups are topologically sorted, so each
/// project comes after all of its dependencies.
///
/// This is an important part of computing a content-addressable hash of
/// projects. For cyclic projects (groups with more than one element), the group
/// is hashed as a whole as a workspace, then each individual project hash uses
/// the workspace hash plus its path in the workspace.
#[expect(clippy::needless_pass_by_value)]
pub(super) fn group_project_nodes(
    projects: &super::Projects,
    project_refs: impl IntoIterator<Item = ProjectRef> + Clone,
) -> Vec<HashSet<ProjectRef>> {
    // Create a copy of the graph, but keeping only project nodes that
    // are reachable from input projects.
    let mut graph = projects.graph.clone();
    let mut dfs_space = petgraph::algo::DfsSpace::default();
    graph.retain_nodes(|graph, index| match &graph[index] {
        crate::project::ProjectNode::Project => {
            project_refs.clone().into_iter().any(|project_ref| {
                petgraph::algo::has_path_connecting(
                    &*graph,
                    project_ref.0,
                    index,
                    Some(&mut dfs_space),
                )
            })
        }
        crate::project::ProjectNode::Workspace
        | crate::project::ProjectNode::Module
        | crate::project::ProjectNode::Static
        | crate::project::ProjectNode::UnresolvedStatic => false,
    });

    // Group nodes by finding the strongly-connected components of the graph.
    let node_groups = petgraph::algo::tarjan_scc(&graph);

    node_groups
        .into_iter()
        .map(|nodes| nodes.into_iter().map(ProjectRef).collect())
        .collect()
}

pub(super) fn hash_projects_inner(
    brioche: &mut BriocheState,
    permit: &mut crate::blob::SaveBlobPermit,
    project_groups: &[HashSet<ProjectRef>],
    project_hashes: &mut HashMap<ProjectRef, ProjectHash>,
    mut project_entries: Option<&mut HashMap<ProjectRef, ContentAddressedProjectEntry>>,
) {
    for project_group in project_groups {
        if project_group.len() == 1 {
            let project_ref = *project_group.iter().next().unwrap();
            let project =
                content_addressed_project(brioche, permit, project_ref, project_hashes, None);
            let project_entry = ContentAddressedProjectEntry::Project(project);

            project_hashes.insert(project_ref, project_entry.project_hash());

            if let Some(project_entries) = &mut project_entries {
                project_entries.insert(project_ref, project_entry);
            }
        } else {
            let mut common_workspace_ref = None;
            let projects_with_paths: HashMap<_, _> = project_group
                .iter()
                .copied()
                .map(|project_ref| {
                    let (workspace_ref, workspace_path) = brioche
                        .projects
                        .graph
                        .edges_directed(project_ref.0, petgraph::Incoming)
                        .find_map(|edge| {
                            let ProjectEdge::ProjectWithinWorkspace(subpath) = edge.weight() else {
                                return None;
                            };

                            let workspace_ref = WorkspaceRef(edge.source());
                            Some((workspace_ref, subpath))
                        })
                        .expect("todo: cyclic node group member isn't in a workspace");
                    let group_workspace_ref = common_workspace_ref.get_or_insert(workspace_ref);
                    assert_eq!(
                        *group_workspace_ref, workspace_ref,
                        "todo: node group members aren't part of the same workspace"
                    );

                    let workspace_path = ContentAddressedWorkspacePath::try_from(workspace_path)
                        .expect("todo: invalid workspace path");
                    (project_ref, workspace_path)
                })
                .collect();

            let members = projects_with_paths
                .iter()
                .map(|(project_ref, workspace_path)| {
                    let project = content_addressed_project(
                        brioche,
                        permit,
                        *project_ref,
                        project_hashes,
                        Some(&projects_with_paths),
                    );
                    (workspace_path.clone(), project)
                })
                .collect();
            let group_workspace = ContentAddressedWorkspace { members };
            let group_workspace_hash = group_workspace.workspace_hash();

            for (project_ref, path) in projects_with_paths {
                let project_entry = ContentAddressedProjectEntry::WorkspaceMember {
                    workspace: group_workspace_hash,
                    path: path.clone(),
                };

                project_hashes.insert(project_ref, project_entry.project_hash());

                if let Some(project_entries) = &mut project_entries {
                    project_entries.insert(project_ref, project_entry);
                }
            }
        }
    }
}

#[expect(clippy::similar_names)]
fn content_addressed_project(
    brioche: &mut BriocheState,
    permit: &mut crate::blob::SaveBlobPermit,
    project_ref: ProjectRef,
    project_hashes: &HashMap<ProjectRef, ProjectHash>,
    workspace_group_siblings: Option<&HashMap<ProjectRef, ContentAddressedWorkspacePath>>,
) -> ContentAddressedProject {
    let project = &brioche.projects.projects[&project_ref];

    let dependencies = brioche
        .projects
        .graph
        .edges(project_ref.0)
        .filter_map(|edge| {
            let crate::project::ProjectEdge::ProjectDependency(dep_name) = edge.weight() else {
                return None;
            };

            let dep_project_ref = ProjectRef(edge.target());

            // Reference the dependency by hash, or by relative workspace path
            // if the dependency is in the same group in the case of cycles
            let dep_ref = workspace_group_siblings
                .and_then(|siblings| siblings.get(&dep_project_ref))
                .map_or_else(
                    || DependencyRef::Project(project_hashes[&dep_project_ref]),
                    |path| DependencyRef::WorkspaceMember { path: path.clone() },
                );
            Some((dep_name.clone(), dep_ref))
        })
        .collect();

    let mut modules = HashMap::<RelativePath, crate::hash::Blake3Hash>::new();
    let mut statics = HashMap::<RelativePath, BTreeMap<StaticQuery, Option<StaticOutput>>>::new();

    for (module_subpath, module_ref) in &brioche.projects.modules_by_project[&project_ref] {
        let module_source = brioche.projects.modules[module_ref]
            .source
            .as_deref()
            .expect("todo: handle module load error");
        let source_hash = blake3::hash(module_source.as_bytes());
        modules.insert(
            module_subpath.clone(),
            crate::hash::Blake3Hash::from(source_hash),
        );

        let module_static_refs = brioche
            .projects
            .graph
            .edges_directed(module_ref.0, petgraph::Direction::Outgoing)
            .filter_map(|edge| {
                if let ProjectEdge::ModuleStatic(query) = edge.weight() {
                    Some((query, StaticRef(edge.target())))
                } else {
                    None
                }
            });
        for (query, static_ref) in module_static_refs {
            let Some(static_) = brioche.projects.get_static(static_ref) else {
                todo!("handle unresolved static");
            };

            let static_output = match static_ {
                super::Static::IncludeFile(_) => {
                    let static_path = brioche
                        .projects
                        .static_path(static_ref)
                        .unwrap()
                        .expect("no local path for include static");
                    let static_path = static_path
                        .to_system_path()
                        .expect("todo: failed to convert static path");

                    // TODO: Wrap with blocking!!
                    let mut artifact = None;
                    crate::recipe::load::load_artifact_sync(
                        &brioche.resources,
                        permit,
                        &mut artifact,
                        &static_path,
                        ArtifactPath::default(),
                    )
                    .expect("todo: load artifact error");
                    let artifact = artifact.unwrap();

                    assert!(
                        matches!(artifact, ArtifactBuilder::File { .. }),
                        "todo: expected file artifact"
                    );

                    let recipe_ref =
                        crate::recipe::build::build_artifact_inner(&mut brioche.recipes, &artifact)
                            .expect("todo: failed to build artifact");
                    let recipe_hash =
                        crate::recipe::hash::hash_recipe_inner(&mut brioche.recipes, recipe_ref);

                    StaticOutput::RecipeHash(recipe_hash)
                }
                super::Static::IncludeDirectory(_) => {
                    let static_path = brioche
                        .projects
                        .static_path(static_ref)
                        .unwrap()
                        .expect("no local path for include static");
                    let static_path = static_path
                        .to_system_path()
                        .expect("todo: failed to convert static path");

                    // TODO: Wrap with blocking!!
                    let mut artifact = None;
                    crate::recipe::load::load_artifact_sync(
                        &brioche.resources,
                        permit,
                        &mut artifact,
                        &static_path,
                        ArtifactPath::default(),
                    )
                    .expect("todo: load artifact error");
                    let artifact = artifact.unwrap();

                    assert!(
                        matches!(artifact, ArtifactBuilder::Directory { .. }),
                        "todo: expected directory artifact"
                    );

                    let recipe_ref =
                        crate::recipe::build::build_artifact_inner(&mut brioche.recipes, &artifact)
                            .expect("todo: failed to build artifact");
                    let recipe_hash =
                        crate::recipe::hash::hash_recipe_inner(&mut brioche.recipes, recipe_ref);

                    StaticOutput::RecipeHash(recipe_hash)
                }
                super::Static::Glob { patterns } => {
                    let static_path = brioche
                        .projects
                        .static_path(static_ref)
                        .unwrap()
                        .expect("no local path for include static");
                    let static_path = static_path
                        .to_system_path()
                        .expect("todo: failed to convert static path");

                    // TODO: Wrap with blocking!!
                    let mut artifact = None;
                    crate::recipe::load::load_artifact_glob_sync(
                        &brioche.resources,
                        permit,
                        &mut artifact,
                        &static_path,
                        &ArtifactPath::default(),
                        patterns,
                    )
                    .expect("todo: load artifact error");
                    let artifact = artifact.unwrap();

                    assert!(
                        matches!(artifact, ArtifactBuilder::Directory { .. }),
                        "todo: expected directory artifact"
                    );

                    let recipe_ref =
                        crate::recipe::build::build_artifact_inner(&mut brioche.recipes, &artifact)
                            .expect("todo: failed to build artifact");
                    let recipe_hash =
                        crate::recipe::hash::hash_recipe_inner(&mut brioche.recipes, recipe_ref);

                    StaticOutput::RecipeHash(recipe_hash)
                }
                super::Static::Download { url: _, hash } => {
                    StaticOutput::Kind(StaticOutputKind::Download { hash: hash.clone() })
                }
                super::Static::GitRef {
                    repository: _,
                    ref_: _,
                    commit,
                } => StaticOutput::Kind(StaticOutputKind::GitRef {
                    commit: commit.clone(),
                }),
            };

            let query = match &query.query {
                super::StaticQuery::IncludeFile(path) => {
                    StaticQuery::Include(StaticInclude::File {
                        path: path.to_string(),
                    })
                }
                super::StaticQuery::IncludeDirectory(path) => {
                    StaticQuery::Include(StaticInclude::Directory {
                        path: path.to_string(),
                    })
                }
                super::StaticQuery::Glob { patterns } => StaticQuery::Glob {
                    patterns: patterns.clone(),
                },
                super::StaticQuery::Download { url } => StaticQuery::Download { url: url.clone() },
                super::StaticQuery::GitRef(options) => StaticQuery::GitRef(GitRefOptions {
                    repository: options.repository.clone(),
                    ref_: options.ref_.clone(),
                }),
            };

            statics
                .entry(module_subpath.clone())
                .or_default()
                .insert(query, Some(static_output));
        }
    }

    ContentAddressedProject {
        definition: project.definition.clone(),
        dependencies,
        modules,
        statics,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ContentAddressedProjectEntry {
    WorkspaceMember {
        workspace: WorkspaceHash,
        path: ContentAddressedWorkspacePath,
    },
    #[serde(untagged)]
    Project(ContentAddressedProject),
}

impl ContentAddressedProjectEntry {
    #[must_use]
    pub fn project_hash(&self) -> ProjectHash {
        let mut hasher = blake3::Hasher::new();
        json_canon::to_writer(&mut hasher, self).expect("failed to serialize project");
        ProjectHash(hasher.finalize().into())
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct WorkspaceHash(crate::hash::Blake3Hash);

impl std::str::FromStr for WorkspaceHash {
    type Err = crate::hash::ParseHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hash = s.parse()?;
        Ok(Self(hash))
    }
}

impl std::fmt::Display for WorkspaceHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentAddressedProject {
    definition: ProjectDefinition,
    dependencies: HashMap<String, DependencyRef>,
    #[serde_as(as = "HashMap<TickEncoded, _>")]
    modules: HashMap<RelativePath, crate::hash::Blake3Hash>,
    #[serde_as(as = "HashMap<TickEncoded, Vec<(_, _)>>")]
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    statics: HashMap<RelativePath, BTreeMap<StaticQuery, Option<StaticOutput>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum DependencyRef {
    WorkspaceMember {
        path: ContentAddressedWorkspacePath,
    },
    #[serde(untagged)]
    Project(ProjectHash),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum StaticQuery {
    Include(StaticInclude),
    Glob { patterns: Vec<String> },
    Download { url: url::Url },
    GitRef(GitRefOptions),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "include")]
#[serde(rename_all = "snake_case")]
enum StaticInclude {
    File { path: String },
    Directory { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
struct GitRefOptions {
    pub repository: url::Url,

    #[serde(rename = "ref")]
    pub ref_: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum StaticOutput {
    RecipeHash(crate::recipe::hash::RecipeHash),
    Kind(StaticOutputKind),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
enum StaticOutputKind {
    Download { hash: crate::hash::AnyHash },
    GitRef { commit: String },
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentAddressedWorkspace {
    members: BTreeMap<ContentAddressedWorkspacePath, ContentAddressedProject>,
}

impl ContentAddressedWorkspace {
    fn workspace_hash(&self) -> WorkspaceHash {
        let mut hasher = blake3::Hasher::new();
        json_canon::to_writer(&mut hasher, self).expect("failed to serialize workspace");
        WorkspaceHash(hasher.finalize().into())
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentAddressedWorkspacePath {
    components: Vec<String>,
}

impl ContentAddressedWorkspacePath {
    #[must_use]
    pub fn parent_with_last_component(&self) -> Option<(Self, String)> {
        let mut parent = self.clone();
        let last = parent.components.pop()?;
        Some((parent, last))
    }
}

impl std::fmt::Display for ContentAddressedWorkspacePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.components.iter().join_with('/'))
    }
}

impl std::str::FromStr for ContentAddressedWorkspacePath {
    type Err = InvalidWorkspacePathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path: RelativePath = s.parse().map_err(|error| match error {})?;
        path.try_into()
    }
}

impl std::fmt::Debug for ContentAddressedWorkspacePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ContentAddressedWorkspacePath({self})")
    }
}

impl serde::Serialize for ContentAddressedWorkspacePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for ContentAddressedWorkspacePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <&str>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl TryFrom<&'_ RelativePath> for ContentAddressedWorkspacePath {
    type Error = InvalidWorkspacePathError;

    fn try_from(value: &RelativePath) -> Result<Self, Self::Error> {
        let components = value
            .components()
            .map(|component| {
                let RelativePathComponent::Normal(component) = component else {
                    return Err(InvalidWorkspacePathError::InvalidPathComponent {
                        path: value.clone(),
                        component: component.clone(),
                    });
                };
                let component = component
                    .to_str()
                    .map_err(|_| InvalidWorkspacePathError::NonStringPath(value.clone()))?;

                if component.is_empty() || component.contains('/') {
                    return Err(InvalidWorkspacePathError::InvalidName(value.clone()));
                } else if component.contains('*') {
                    return Err(InvalidWorkspacePathError::UnexpectedWildcard(value.clone()));
                }

                Ok(component.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { components })
    }
}

impl TryFrom<RelativePath> for ContentAddressedWorkspacePath {
    type Error = InvalidWorkspacePathError;

    fn try_from(value: RelativePath) -> Result<Self, Self::Error> {
        (&value).try_into()
    }
}

impl From<ContentAddressedWorkspacePath> for RelativePath {
    fn from(value: ContentAddressedWorkspacePath) -> Self {
        (&value).into()
    }
}

impl From<&'_ ContentAddressedWorkspacePath> for RelativePath {
    fn from(value: &ContentAddressedWorkspacePath) -> Self {
        value
            .components
            .iter()
            .map(|component| {
                RelativePathComponent::new(component)
                    .expect("workspace path contained invalid component")
            })
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidWorkspacePathError {
    #[error("invalid component in path '{path}': '{component}'")]
    InvalidPathComponent {
        path: RelativePath,
        component: RelativePathComponent,
    },

    #[error("unexpected wildcard in path '{0}'")]
    UnexpectedWildcard(RelativePath),

    #[error("path '{0}' contains a component that cannot be represented as a string")]
    NonStringPath(RelativePath),

    #[error("path '{0}' contains an invalid character")]
    InvalidName(RelativePath),
}
