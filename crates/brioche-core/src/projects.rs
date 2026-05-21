use std::collections::{BTreeMap, HashMap};

use petgraph::{stable_graph::NodeIndex, visit::EdgeRef as _};

use crate::{
    Brioche,
    path::{AbsolutePath, AnyPath, RelativePath},
    registry::RegistryError,
    script::specifier::ImportSpecifier,
};

mod artifact;
pub mod debug;
pub mod hash;
pub mod load;

type ProjectGraph = petgraph::stable_graph::StableDiGraph<ProjectNode, ProjectEdge>;

#[derive(Default)]
pub struct Projects {
    graph: ProjectGraph,
    projects: HashMap<ProjectRef, Project>,
    modules: HashMap<ModuleRef, Module>,
    workspaces: HashMap<WorkspaceRef, Result<Workspace, load::LoadWorkspaceError>>,
    projects_by_specifier: HashMap<ProjectSpecifier, ProjectRef>,
    modules_by_project: HashMap<ProjectRef, HashMap<RelativePath, ModuleRef>>,
    workspaces_by_path: HashMap<AbsolutePath, WorkspaceRef>,
    issues: HashMap<NodeIndex, Vec<ProjectIssue>>,
}

#[derive(Debug, Clone)]
pub(crate) enum ProjectNode {
    Workspace,
    Project,
    Module,
}

#[derive(Debug, Clone)]
pub(crate) enum ProjectEdge {
    ProjectWithinWorkspace,
    ProjectDependency(String),
    ProjectRootModule,
    ModuleImport(ImportSpecifier),
}

#[derive(Clone)]
pub struct Project {
    pub definition: ProjectDefinition,
    pub specifier: ProjectSpecifier,
}

pub(crate) struct Module {
    subpath: RelativePath,
    source: Result<String, load::LoadModuleError>,
}

pub(crate) struct Workspace {
    root: AbsolutePath,
    definition: WorkspaceDefinition,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDefinition {
    pub name: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub dependencies: HashMap<String, DependencyDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum DependencyDefinition {
    Path { path: String },
    Version(Version),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceDefinition {
    pub members: Vec<WorkspaceMember>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lockfile {
    pub dependencies: BTreeMap<String, hash::ProjectHash>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub downloads: BTreeMap<url::Url, crate::hash::AnyHash>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub git_refs: BTreeMap<url::Url, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, serde_with::SerializeDisplay, serde_with::DeserializeFromStr)]
pub enum WorkspaceMember {
    Path(RelativePath, String),
    WildcardPath(RelativePath),
}

impl std::str::FromStr for WorkspaceMember {
    type Err = load::WorkspaceMemberParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(wildcard_path) = s.strip_suffix("/*") {
            if wildcard_path.contains('*') {
                return Err(load::WorkspaceMemberParseError::InvalidGlobPattern);
            }

            let wildcard_path = RelativePath::new(wildcard_path).normalized_subpath()?;
            Ok(Self::WildcardPath(wildcard_path))
        } else {
            if s.contains('*') {
                return Err(load::WorkspaceMemberParseError::InvalidGlobPattern);
            }

            let (parent, name) = match s.split_once('/') {
                Some((parent, name)) => (RelativePath::new(parent), name),
                None => (RelativePath::default(), s),
            };

            Ok(Self::Path(parent, name.to_string()))
        }
    }
}

impl std::fmt::Display for WorkspaceMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(path, name) => write!(f, "{path}/{name}"),
            Self::WildcardPath(path) => write!(f, "{path}/*"),
        }
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, serde_with::DeserializeFromStr, serde_with::SerializeDisplay,
)]
pub enum Version {
    Any,
}

impl std::str::FromStr for Version {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "*" => Ok(Self::Any),
            _ => anyhow::bail!("unsupported version specifier: {s}"),
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any => write!(f, "*"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectRef(NodeIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ModuleRef(NodeIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WorkspaceRef(NodeIndex);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectSpecifier {
    Path(AbsolutePath),
    Hash(hash::ProjectHash),
}

#[derive(Debug)]
enum ProjectReferrer {
    TopLevel,
    Project {
        referrer: ProjectRef,
        edge: ProjectEdge,
        location: ProjectIssueLocation,
    },
}

#[derive(Debug)]
enum ModuleReferrer {
    ProjectRoot {
        project_ref: ProjectRef,
    },
    ModuleImport {
        referrer: ModuleRef,
        specifier: ImportSpecifier,
        location: ProjectIssueLocation,
    },
}

impl ModuleReferrer {
    const fn node_index(&self) -> NodeIndex {
        match self {
            Self::ProjectRoot { project_ref } => project_ref.0,
            Self::ModuleImport { referrer, .. } => referrer.0,
        }
    }

    fn edge(&self) -> ProjectEdge {
        match self {
            Self::ProjectRoot { .. } => ProjectEdge::ProjectRootModule,
            Self::ModuleImport { specifier, .. } => ProjectEdge::ModuleImport(specifier.clone()),
        }
    }
}

pub async fn get_dependencies(
    brioche: &Brioche,
    project_ref: ProjectRef,
) -> HashMap<String, ProjectRef> {
    let projects = brioche.projects.read().await;

    projects
        .graph
        .edges(project_ref.0)
        .filter_map(|edge| {
            let ProjectEdge::ProjectDependency(dep_name) = edge.weight() else {
                return None;
            };

            let dep_ref = ProjectRef(edge.target());
            Some((dep_name.clone(), dep_ref))
        })
        .collect()
}

pub async fn get_specifier(brioche: &Brioche, project_ref: ProjectRef) -> ProjectSpecifier {
    let projects = brioche.projects.read().await;

    projects.projects[&project_ref].specifier.clone()
}

pub async fn get_all_issues(brioche: &Brioche) -> Vec<ProjectIssue> {
    let projects = brioche.projects.read().await;

    projects
        .issues
        .values()
        .flat_map(|issues| issues.iter().cloned())
        .collect()
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProjectIssue {
    #[error("{error}")]
    ScriptParseError {
        error: crate::script::parse::ScriptParseError,
        path: AbsolutePath,
    },

    #[error("{error}")]
    LoadModuleError {
        error: load::LoadModuleError,
        path: AbsolutePath,
        location: Option<ProjectIssueLocation>,
    },

    #[error("invalid project definition: {error_message}")]
    InvalidProjectDefinition {
        error_message: String,
        line: usize,
        column: usize,
        location: ProjectIssueLocation,
    },

    #[error("IO error at {path}: {error_message}")]
    IoError {
        error_message: String,
        path: AbsolutePath,
        location: ProjectIssueLocation,
    },

    #[error("registry error: {error}")]
    RegistryError {
        #[source]
        error: RegistryError,
        location: ProjectIssueLocation,
    },

    #[error("cache error: {error_message}")]
    CacheError {
        // TODO: Use proper error
        error_message: String,
    },

    #[error("invalid path '{path}': {error}")]
    ToSystemPathError {
        #[source]
        error: crate::path::ToSystemPathError,
        path: AnyPath,
        location: ProjectIssueLocation,
    },

    #[error("module import '{}' escapes project path", import.specifier)]
    ModuleImportEscapesProjectPath {
        import: crate::script::parse::ScriptImport,
        path: AbsolutePath,
    },
}

impl ProjectIssue {
    #[must_use]
    pub fn location(&self) -> Option<ProjectIssueLocation> {
        match self {
            Self::InvalidProjectDefinition { location, .. }
            | Self::IoError { location, .. }
            | Self::RegistryError { location, .. }
            | Self::ToSystemPathError { location, .. } => Some(location.clone()),
            Self::LoadModuleError { location, .. } => location.clone(),
            Self::ScriptParseError { error, path } => Some(ProjectIssueLocation {
                path: path.clone(),
                range: Some(error.range()),
            }),
            Self::ModuleImportEscapesProjectPath { import, path } => Some(ProjectIssueLocation {
                path: path.clone(),
                range: Some(import.range),
            }),
            Self::CacheError { .. } => {
                // TODO: Track location
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectIssueLocation {
    pub path: AbsolutePath,
    pub range: Option<crate::script::parse::TextRange>,
}
