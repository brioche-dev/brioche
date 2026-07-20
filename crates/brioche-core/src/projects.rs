use std::collections::{BTreeMap, HashMap};

use petgraph::{stable_graph::NodeIndex, visit::EdgeRef as _};

use crate::{
    Brioche,
    hash::AnyHash,
    path::{AbsolutePath, AnyPath, RelativePath},
    projects::hash::ProjectHash,
    registry::RegistryError,
    script::{parse::ModuleStaticQuery, specifier::ImportSpecifier},
};

pub mod artifact;
pub mod debug;
pub mod hash;
pub mod load;

type ProjectGraph = petgraph::stable_graph::StableDiGraph<ProjectNode, ProjectEdge>;

#[derive(Default)]
pub struct Projects {
    graph: ProjectGraph,
    workspaces: HashMap<WorkspaceRef, Result<Workspace, load::LoadWorkspaceError>>,
    projects: HashMap<ProjectRef, Project>,
    modules: HashMap<ModuleRef, Module>,
    statics: HashMap<StaticRef, Static>,
    unresolved_statics: HashMap<StaticRef, UnresolvedStatic>,
    resolved_statics: HashMap<StaticRef, StaticRef>,
    projects_by_specifier: HashMap<ProjectSpecifier, ProjectRef>,
    local_project_paths: HashMap<ProjectRef, AbsolutePath>,
    modules_by_project: HashMap<ProjectRef, HashMap<RelativePath, ModuleRef>>,
    project_by_module: HashMap<ModuleRef, (ProjectRef, RelativePath)>,
    workspaces_by_path: HashMap<AbsolutePath, WorkspaceRef>,
    static_ref_by_shared_static: HashMap<SharedStatic, StaticRef>,
    static_ref_by_unresolved_static:
        HashMap<UnresolvedStatic, (StaticRef, Vec<ProjectIssueLocation>)>,
    issues: HashMap<NodeIndex, Vec<ProjectIssue>>,
}

impl Projects {
    pub(crate) fn module_statics(
        &self,
        module: ModuleRef,
    ) -> impl Iterator<Item = (&ModuleStaticQuery, StaticRef)> {
        self.graph
            .edges_directed(module.0, petgraph::Direction::Outgoing)
            .filter_map(|edge| {
                if let ProjectEdge::ModuleStatic(query) = edge.weight() {
                    Some((query, StaticRef(edge.target())))
                } else {
                    None
                }
            })
    }

    pub(crate) fn module_for_static(&self, static_ref: StaticRef) -> Option<ModuleRef> {
        self.graph
            .edges_directed(static_ref.0, petgraph::Direction::Incoming)
            .find_map(|edge| {
                if let ProjectEdge::ModuleStatic(_) = edge.weight() {
                    Some(ModuleRef(edge.source()))
                } else {
                    None
                }
            })
    }

    pub(crate) fn get_static(&self, static_ref: StaticRef) -> Option<&Static> {
        self.statics
            .get(&static_ref)
            .or_else(|| self.statics.get(self.resolved_statics.get(&static_ref)?))
    }

    #[expect(clippy::result_large_err)]
    pub(crate) fn static_path(
        &self,
        static_ref: StaticRef,
    ) -> Result<Option<AbsolutePath>, ProjectIssue> {
        let module_ref = self.module_for_static(static_ref);
        let Some(module_ref) = module_ref else {
            return Ok(None);
        };

        let Some(static_) = self.get_static(static_ref) else {
            return Ok(None);
        };

        match static_ {
            Static::IncludeFile(relative_path) | Static::IncludeDirectory(relative_path) => {
                let (project_ref, module_subpath) = &self.project_by_module[&module_ref];
                let module_dir = module_subpath.parent().expect("invalid module subpath");
                let project_path = &self.local_project_paths[project_ref];
                let static_subpath = module_dir.join(relative_path.clone());
                let static_path = project_path.join_subpath(static_subpath);
                let Ok(static_path) = static_path else {
                    return Err(ProjectIssue::StaticIncludeEscapesProjectPath {
                        include: relative_path.clone(),
                        module_subpath: module_subpath.clone(),
                    });
                };

                Ok(Some(static_path))
            }
            Static::Glob { .. } => {
                let (project_ref, module_subpath) = &self.project_by_module[&module_ref];
                let module_dir = module_subpath.parent().expect("invalid module subpath");
                let project_path = &self.local_project_paths[project_ref];
                let static_path = project_path.join_subpath(module_dir);
                let Ok(static_path) = static_path else {
                    unreachable!("invlaid module dir path");
                };

                Ok(Some(static_path))
            }
            Static::Download { .. } | Static::GitRef { .. } => Ok(None),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ProjectNode {
    Workspace,
    Project,
    Module,
    Static,
    UnresolvedStatic,
}

#[derive(Debug, Clone)]
pub(crate) enum ProjectEdge {
    ProjectWithinWorkspace,
    ProjectDependency(String),
    ProjectRootModule,
    ModuleImport(ImportSpecifier),
    ModuleStatic(ModuleStaticQuery),
    ResolvedStatic,
}

#[derive(Clone)]
pub struct Project {
    pub definition: ProjectDefinition,
    pub specifier: ProjectSpecifier,
    pub lockfile: Lockfile,
}

pub(crate) struct Module {
    project: ProjectRef,
    subpath: RelativePath,
    source: Result<String, load::LoadModuleError>,
}

pub(crate) struct Workspace {
    root: AbsolutePath,
    definition: WorkspaceDefinition,
}

#[derive(Debug, Clone)]
pub(crate) enum Static {
    IncludeFile(RelativePath),
    IncludeDirectory(RelativePath),
    Glob {
        patterns: Vec<String>,
    },
    Download {
        url: url::Url,
        hash: AnyHash,
    },
    GitRef {
        repository: url::Url,
        ref_: String,
        commit: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SharedStatic {
    Download {
        url: url::Url,
        hash: AnyHash,
    },
    GitRef {
        repository: url::Url,
        ref_: String,
        commit: String,
    },
}

impl From<SharedStatic> for Static {
    fn from(value: SharedStatic) -> Self {
        match value {
            SharedStatic::Download { url, hash } => Self::Download { url, hash },
            SharedStatic::GitRef {
                repository,
                ref_,
                commit,
            } => Self::GitRef {
                repository,
                ref_,
                commit,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum UnresolvedStatic {
    Download { url: url::Url },
    GitRef { repository: url::Url, ref_: String },
}

#[derive(Debug, Clone)]
pub(crate) enum StaticQuery {
    IncludeFile(RelativePath),
    IncludeDirectory(RelativePath),
    Glob { patterns: Vec<String> },
    Download { url: url::Url },
    GitRef(ModuleStaticQueryGitRefOptions),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModuleStaticQueryGitRefOptions {
    pub repository: url::Url,

    #[serde(rename = "ref")]
    pub ref_: String,
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
pub(crate) struct StaticRef(NodeIndex);

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

pub async fn local_project_path(brioche: &Brioche, project_ref: ProjectRef) -> AbsolutePath {
    let projects = brioche.projects.read().await;
    projects.local_project_paths[&project_ref].clone()
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

pub async fn get_project_by_specifier(
    brioche: &Brioche,
    project_specifier: &ProjectSpecifier,
) -> Option<ProjectRef> {
    let projects = brioche.projects.read().await;

    projects
        .projects_by_specifier
        .get(project_specifier)
        .copied()
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

    #[error("error downloading URL '{url}': {error_message}")]
    DownloadError {
        url: url::Url,

        // TODO: Use proper error
        error_message: String,

        location: ProjectIssueLocation,
    },

    #[error("expected project with hash {expected_hash}, but got {actual_hash}")]
    ProjectHashMismatch {
        expected_hash: ProjectHash,
        actual_hash: ProjectHash,
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

    #[error("static include '{include}' escapes project path")]
    StaticIncludeEscapesProjectPath {
        include: RelativePath,
        module_subpath: RelativePath,
    },

    #[error("expected static include '{include}' to be a file")]
    StaticIncludeExpectedFile {
        include: RelativePath,
        module_subpath: RelativePath,
    },

    #[error("expected static include '{include}' to be a directory")]
    StaticIncludeExpectedDirectory {
        include: RelativePath,
        module_subpath: RelativePath,
    },
}

impl ProjectIssue {
    #[must_use]
    pub fn location(&self) -> Option<ProjectIssueLocation> {
        match self {
            Self::LoadModuleError { location, .. } => location.clone(),
            Self::ScriptParseError { error, path } => Some(ProjectIssueLocation {
                path: path.clone(),
                range: Some(error.range()),
            }),
            Self::ModuleImportEscapesProjectPath { import, path } => Some(ProjectIssueLocation {
                path: path.clone(),
                range: Some(import.range),
            }),
            Self::CacheError { .. }
            | Self::StaticIncludeEscapesProjectPath { .. }
            | Self::StaticIncludeExpectedFile { .. }
            | Self::StaticIncludeExpectedDirectory { .. } => {
                // TODO: Track location
                None
            }
            Self::InvalidProjectDefinition { location, .. }
            | Self::IoError { location, .. }
            | Self::RegistryError { location, .. }
            | Self::ToSystemPathError { location, .. }
            | Self::DownloadError { location, .. } => Some(location.clone()),
            Self::ProjectHashMismatch { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectIssueLocation {
    pub path: AbsolutePath,
    pub range: Option<crate::script::parse::TextRange>,
}
