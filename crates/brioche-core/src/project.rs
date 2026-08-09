use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use petgraph::{stable_graph::NodeIndex, visit::EdgeRef as _};

use crate::{
    BriocheState,
    hash::AnyHash,
    path::{AbsolutePath, AnyPath, RelativePath},
    project::hash::ProjectHash,
    registry::RegistryError,
    script::{
        parse::{ModuleStaticQuery, TextRange},
        specifier::ImportSpecifier,
    },
};

pub mod artifact;
pub mod debug;
pub mod hash;
pub mod load;
pub mod lock;

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

    pub(crate) fn module_for_static(
        &self,
        static_ref: StaticRef,
    ) -> Option<(ModuleRef, TextRange)> {
        self.graph
            .edges_directed(static_ref.0, petgraph::Direction::Incoming)
            .find_map(|edge| {
                if let ProjectEdge::ModuleStatic(query) = edge.weight() {
                    Some((ModuleRef(edge.source()), query.range))
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
        let Some((module_ref, range)) = module_ref else {
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
                let static_path = project_path.join_subpath(&static_subpath);
                let Ok(static_path) = static_path else {
                    return Err(ProjectIssue::StaticIncludeEscapesProjectPath {
                        static_ref,
                        include: relative_path.clone(),
                        module_ref,
                        range,
                    });
                };

                Ok(Some(static_path))
            }
            Static::Glob { .. } => {
                let (project_ref, module_subpath) = &self.project_by_module[&module_ref];
                let module_dir = module_subpath.parent().expect("invalid module subpath");
                let project_path = &self.local_project_paths[project_ref];
                let static_path = project_path.join_subpath(&module_dir);
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
    ProjectWithinWorkspace(RelativePath),
    ProjectDependency(String),
    ProjectRootModule,
    ModuleImport(ImportSpecifier),
    ModuleStatic(ModuleStaticQuery),
    ResolvedStatic,
}

pub struct Project {
    pub definition: ProjectDefinition,
    pub specifier: ProjectSpecifier,
    pub lockfile_state: LockfileState,
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

pub enum LockfileState {
    Clean(Lockfile),
    Dirty {
        old: Option<Lockfile>,
        new: Lockfile,
    },
}

impl LockfileState {
    #[must_use]
    pub fn new(
        old_lockfile_with_content: Option<(Lockfile, &[u8])>,
        new_lockfile: Lockfile,
    ) -> Self {
        let Some((old_lockfile, old_lockfile_content)) = old_lockfile_with_content else {
            return Self::Dirty {
                old: None,
                new: new_lockfile,
            };
        };
        if old_lockfile != new_lockfile {
            return Self::Dirty {
                old: Some(old_lockfile),
                new: new_lockfile,
            };
        }

        let new_lockfile_content =
            serde_json::to_vec_pretty(&new_lockfile).expect("failed to serialize lockfile");
        if old_lockfile_content == new_lockfile_content {
            Self::Clean(new_lockfile)
        } else {
            Self::Dirty {
                old: Some(old_lockfile),
                new: new_lockfile,
            }
        }
    }

    pub fn update(&mut self, mut f: impl FnMut(&mut Lockfile)) {
        match self {
            Self::Clean(lockfile) => {
                let mut new_lockfile = lockfile.clone();
                f(&mut new_lockfile);
                if new_lockfile != *lockfile {
                    *self = Self::Dirty {
                        old: Some(std::mem::take(lockfile)),
                        new: new_lockfile,
                    };
                }
            }
            Self::Dirty { old: _, new } => {
                f(new);
            }
        }
    }

    #[must_use]
    pub const fn lockfile(&self) -> &Lockfile {
        match self {
            Self::Clean(lockfile) => lockfile,
            Self::Dirty { old: _, new } => new,
        }
    }
}

#[derive(Debug, Clone, serde_with::SerializeDisplay, serde_with::DeserializeFromStr)]
pub enum WorkspaceMember {
    Path(RelativePath, String),
    WildcardPath(RelativePath),
}

impl std::str::FromStr for WorkspaceMember {
    type Err = WorkspaceMemberParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(wildcard_path) = s.strip_suffix("/*") {
            if wildcard_path.contains('*') {
                return Err(WorkspaceMemberParseError::InvalidGlobPattern);
            }

            let wildcard_path = RelativePath::new(wildcard_path).normalized_subpath()?;
            Ok(Self::WildcardPath(wildcard_path))
        } else {
            if s.contains('*') {
                return Err(WorkspaceMemberParseError::InvalidGlobPattern);
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
    type Err = VersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "*" => Ok(Self::Any),
            _ => Err(VersionParseError::InvalidVersionSpecifier(s.to_string())),
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
pub struct ModuleRef(NodeIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StaticRef(NodeIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceRef(NodeIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnyRef {
    Project(ProjectRef),
    Module(ModuleRef),
    Static(StaticRef),
    Workspace(WorkspaceRef),
}

impl From<ProjectRef> for AnyRef {
    fn from(value: ProjectRef) -> Self {
        Self::Project(value)
    }
}

impl From<ModuleRef> for AnyRef {
    fn from(value: ModuleRef) -> Self {
        Self::Module(value)
    }
}

impl From<StaticRef> for AnyRef {
    fn from(value: StaticRef) -> Self {
        Self::Static(value)
    }
}

impl From<WorkspaceRef> for AnyRef {
    fn from(value: WorkspaceRef) -> Self {
        Self::Workspace(value)
    }
}

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
        edge: Box<ProjectEdge>,
        module_referrer: (ModuleRef, Option<TextRange>),
    },
}

#[derive(Debug, Clone)]
enum ModuleReferrer {
    ProjectRoot {
        project_ref: ProjectRef,
        referrer: Option<(ModuleRef, Option<TextRange>)>,
    },
    ModuleImport {
        specifier: ImportSpecifier,
        referrer: (ModuleRef, TextRange),
    },
}

impl ModuleReferrer {
    const fn node_index(&self) -> NodeIndex {
        match self {
            Self::ProjectRoot { project_ref, .. } => project_ref.0,
            Self::ModuleImport {
                referrer: (module_ref, _),
                ..
            } => module_ref.0,
        }
    }

    fn edge(&self) -> ProjectEdge {
        match self {
            Self::ProjectRoot { .. } => ProjectEdge::ProjectRootModule,
            Self::ModuleImport { specifier, .. } => ProjectEdge::ModuleImport(specifier.clone()),
        }
    }

    const fn referrer_and_range(&self) -> Option<(ModuleRef, Option<TextRange>)> {
        match self {
            Self::ProjectRoot { referrer, .. } => *referrer,
            Self::ModuleImport {
                referrer: (module_ref, range),
                ..
            } => Some((*module_ref, Some(*range))),
        }
    }
}

#[must_use]
pub fn local_project_path(brioche: &BriocheState, project_ref: ProjectRef) -> AbsolutePath {
    brioche.projects.local_project_paths[&project_ref].clone()
}

#[must_use]
pub fn get_root_module(brioche: &BriocheState, project_ref: ProjectRef) -> Option<ModuleRef> {
    brioche
        .projects
        .graph
        .edges(project_ref.0)
        .find_map(|edge| {
            if matches!(edge.weight(), ProjectEdge::ProjectRootModule) {
                Some(ModuleRef(edge.target()))
            } else {
                None
            }
        })
}

#[must_use]
pub fn get_dependencies(
    brioche: &BriocheState,
    project_ref: ProjectRef,
) -> HashMap<String, ProjectRef> {
    brioche
        .projects
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

#[must_use]
pub fn get_workspace_membership(
    brioche: &BriocheState,
    project_ref: ProjectRef,
) -> Option<(WorkspaceRef, RelativePath)> {
    brioche
        .projects
        .graph
        .edges_directed(project_ref.0, petgraph::Incoming)
        .find_map(|edge| {
            let ProjectEdge::ProjectWithinWorkspace(subpath) = edge.weight() else {
                return None;
            };

            let workspace_ref = WorkspaceRef(edge.source());
            Some((workspace_ref, subpath.clone()))
        })
}

#[must_use]
pub fn get_specifier(brioche: &BriocheState, project_ref: ProjectRef) -> ProjectSpecifier {
    brioche.projects.projects[&project_ref].specifier.clone()
}

#[must_use]
pub fn get_project_by_specifier(
    brioche: &BriocheState,
    project_specifier: &ProjectSpecifier,
) -> Option<ProjectRef> {
    brioche
        .projects
        .projects_by_specifier
        .get(project_specifier)
        .copied()
}

pub fn get_all_issues(brioche: &BriocheState) -> impl Iterator<Item = &ProjectIssue> {
    brioche.projects.issues.values().flatten()
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectIssue {
    #[error("{error}")]
    ScriptParseError {
        #[source]
        error: crate::script::parse::ScriptParseError,
        module_ref: ModuleRef,
    },

    #[error("{error}")]
    LoadModuleError {
        #[source]
        error: load::LoadModuleError,
        module_ref: ModuleRef,
        referrer: Option<(ModuleRef, Option<TextRange>)>,
    },

    #[error("invalid project definition: {error_message}")]
    InvalidProjectDefinition {
        error_message: String,
        line: usize,
        column: usize,
        location: ProjectIssueLocation,
    },

    #[error("{reason}: {error}")]
    IoError {
        #[source]
        error: std::io::Error,
        reason: Cow<'static, str>,
        location: ProjectIssueLocation,
    },

    #[error("registry error: {error}")]
    RegistryError {
        #[source]
        error: RegistryError,
        location: ProjectIssueLocation,
    },

    #[error("cache error: {error}")]
    CacheError {
        #[source]
        error: crate::cache::CacheError,
        location: ProjectIssueLocation,
    },

    #[error("failed to load project hash {project_hash}: {error}")]
    LoadProjectByHashError {
        #[source]
        error: load::LoadProjectByHashError,
        project_hash: ProjectHash,
        location: ProjectIssueLocation,
    },

    #[error("error downloading URL '{url}': {error}")]
    DownloadError {
        #[source]
        error: crate::download::DownloadError,
        url: url::Url,
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
        location: ProjectIssueLocation,
    },

    #[error("static include '{include}' escapes project path")]
    StaticIncludeEscapesProjectPath {
        static_ref: StaticRef,
        include: RelativePath,
        module_ref: ModuleRef,
        range: TextRange,
    },

    #[error("expected static include '{include}' to be a file")]
    StaticIncludeExpectedFile {
        static_ref: StaticRef,
        include: RelativePath,
        module_ref: ModuleRef,
        range: TextRange,
    },

    #[error("expected static include '{include}' to be a directory")]
    StaticIncludeExpectedDirectory {
        static_ref: StaticRef,
        include: RelativePath,
        module_ref: ModuleRef,
        range: TextRange,
    },

    #[error("dependency not found: '{dependency}'")]
    DependencyNotFound {
        dependency: String,
        location: ProjectIssueLocation,
    },

    #[error("encountered error while validating project with hash {expected_hash}")]
    FailedToValidateHash {
        #[source]
        error: Arc<hash::ContentAddressedProjectError>,
        expected_hash: ProjectHash,
    },
}

impl ProjectIssue {
    #[must_use]
    pub fn location(&self) -> Option<ProjectIssueLocation> {
        match self {
            Self::ScriptParseError { error, module_ref } => Some(ProjectIssueLocation {
                source: (*module_ref).into(),
                range: Some(error.range()),
            }),
            Self::LoadModuleError {
                error: _,
                module_ref: _,
                referrer,
            } => referrer.map(|(module_ref, range)| ProjectIssueLocation {
                source: module_ref.into(),
                range,
            }),
            Self::StaticIncludeEscapesProjectPath {
                module_ref, range, ..
            }
            | Self::StaticIncludeExpectedFile {
                module_ref, range, ..
            }
            | Self::StaticIncludeExpectedDirectory {
                module_ref, range, ..
            } => Some(ProjectIssueLocation {
                source: (*module_ref).into(),
                range: Some(*range),
            }),
            Self::InvalidProjectDefinition { location, .. }
            | Self::IoError { location, .. }
            | Self::RegistryError { location, .. }
            | Self::CacheError { location, .. }
            | Self::ToSystemPathError { location, .. }
            | Self::ModuleImportEscapesProjectPath { location, .. }
            | Self::DownloadError { location, .. }
            | Self::LoadProjectByHashError { location, .. }
            | Self::DependencyNotFound { location, .. } => Some(*location),
            Self::ProjectHashMismatch { .. } | Self::FailedToValidateHash { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProjectIssueLocation {
    pub source: AnyRef,
    pub range: Option<crate::script::parse::TextRange>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceMemberParseError {
    #[error("invalid glob pattern in workspace member path")]
    InvalidGlobPattern,

    #[error(transparent)]
    SubpathError(#[from] crate::path::SubpathError),
}

#[derive(Debug, thiserror::Error)]
pub enum VersionParseError {
    #[error("invalid version specifier '{0}'")]
    InvalidVersionSpecifier(String),
}
