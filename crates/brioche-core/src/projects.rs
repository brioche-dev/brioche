use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::Path,
};

use petgraph::{stable_graph::NodeIndex, visit::EdgeRef as _};

use crate::{
    Brioche,
    path::{AbsolutePath, AnyPath, RelativePath},
    script::specifier::{ImportSpecifier, LocalImportSpecifier},
};

#[derive(Default)]
pub struct Projects {
    graph: petgraph::stable_graph::StableDiGraph<ProjectNode, ProjectEdge>,
    projects: HashMap<ProjectRef, Project>,
    modules: HashMap<ModuleRef, Result<Module, LoadModuleError>>,
    workspaces: HashMap<WorkspaceRef, Result<Workspace, LoadWorkspaceError>>,
    projects_by_specifier: HashMap<ProjectSpecifier, ProjectRef>,
    workspaces_by_path: HashMap<AbsolutePath, WorkspaceRef>,
    issues: HashMap<NodeIndex, Vec<LoadProjectIssue>>,
}

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
    ast: crate::script::parse::ScriptAst,
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
    pub dependencies: BTreeMap<String, ProjectHash>,

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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ProjectHash(crate::hash::Blake3Hash);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectRef(NodeIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ModuleRef(NodeIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WorkspaceRef(NodeIndex);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectSpecifier {
    Path(AbsolutePath),
    Hash(ProjectHash),
}

#[derive(Debug)]
enum ProjectReferrer {
    TopLevel,
    Project {
        referrer: ProjectRef,
        edge: ProjectEdge,
        location: IssueLocation,
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
        location: IssueLocation,
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

#[tracing::instrument(skip_all)]
pub async fn load_projects(
    brioche: &Brioche,
    specifiers: impl IntoIterator<Item = ProjectSpecifier>,
) -> Result<HashMap<ProjectSpecifier, ProjectRef>, LoadProjectError> {
    let mut queue = specifiers
        .into_iter()
        .map(|specifier| (specifier, ProjectReferrer::TopLevel))
        .collect::<VecDeque<_>>();
    let mut projects = brioche.projects.write().await;
    let projects = &mut *projects;
    let mut results = HashMap::new();

    while let Some((specifier, referrer)) = queue.pop_front() {
        if let Some(project) = projects.projects_by_specifier.get(&specifier) {
            tracing::trace!(?specifier, ?referrer, "project already loaded");

            match referrer {
                ProjectReferrer::TopLevel => {
                    results.insert(specifier, *project);
                }
                ProjectReferrer::Project { referrer, edge, .. } => {
                    projects.graph.add_edge(referrer.0, project.0, edge);
                }
            }

            continue;
        }

        tracing::debug!(?specifier, ?referrer, "loading project");

        let (project_path, workspace_root) = match &specifier {
            ProjectSpecifier::Path(path) => {
                let workspace_root = find_workspace_root(path).await?;

                tracing::trace!(?path, ?workspace_root, "searched for workspace root");

                (path, workspace_root)
            }
            ProjectSpecifier::Hash(_project_hash) => {
                todo!("load project by hash")
            }
        };

        let project_ref = projects.graph.add_node(ProjectNode::Project);
        let project_ref = ProjectRef(project_ref);

        projects
            .projects_by_specifier
            .insert(specifier.clone(), project_ref);

        match &referrer {
            ProjectReferrer::TopLevel => {
                results.insert(specifier.clone(), project_ref);
            }
            ProjectReferrer::Project { referrer, edge, .. } => {
                projects
                    .graph
                    .add_edge(referrer.0, project_ref.0, edge.clone());
            }
        }

        let workspace_entry;
        let workspace = if let Some(workspace_root) = workspace_root {
            match projects.workspaces_by_path.entry(workspace_root) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    let workspace_ref = *entry.get();
                    projects.workspaces[&workspace_ref].as_ref().ok()
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let workspace_ref = projects.graph.add_node(ProjectNode::Workspace);
                    let workspace_ref = WorkspaceRef(workspace_ref);

                    projects.graph.add_edge(
                        workspace_ref.0,
                        project_ref.0,
                        ProjectEdge::ProjectWithinWorkspace,
                    );

                    let workspace = load_workspace(entry.key().clone()).await;

                    tracing::trace!(workspace = ?workspace.as_ref().map(|_| ()), "loaded new workspace");

                    entry.insert(workspace_ref);

                    workspace_entry = projects
                        .workspaces
                        .entry(workspace_ref)
                        .insert_entry(workspace);
                    workspace_entry.get().as_ref().ok()
                }
            }
        } else {
            None
        };

        let root_module_subpath = RelativePath::one("project.bri");
        let root_module_path = project_path
            .join_subpath(root_module_subpath.clone())
            .unwrap();

        let mut module_queue = VecDeque::from_iter([(
            root_module_subpath.clone(),
            ModuleReferrer::ProjectRoot { project_ref },
        )]);
        let mut project_modules = HashMap::<RelativePath, ModuleRef>::new();

        while let Some((module_subpath, module_referrer)) = module_queue.pop_front() {
            if let Some(module_ref) = project_modules.get(&module_subpath) {
                projects.graph.add_edge(
                    module_referrer.node_index(),
                    module_ref.0,
                    module_referrer.edge(),
                );
                continue;
            }

            let Some(module_dir) = module_subpath.parent() else {
                panic!("module path does not have a parent: {module_subpath}");
            };
            let module_path = project_path
                .join_subpath(module_subpath.clone())
                .unwrap_or_else(|error| {
                    panic!("module subpath {module_subpath} escapes project path {project_path}: {error}")
                });

            let module_ref = projects.graph.add_node(ProjectNode::Module);
            let module_ref = ModuleRef(module_ref);
            projects.graph.add_edge(
                module_referrer.node_index(),
                module_ref.0,
                module_referrer.edge(),
            );
            project_modules.insert(module_subpath, module_ref);

            let module_system_path = module_path.to_system_path()?;
            let module = load_module(&module_system_path).await;

            let module_entry = projects.modules.entry(module_ref).insert_entry(module);
            let module = module_entry.get();

            match module {
                Ok(module) => {
                    let imports = crate::script::parse::find_imports(&module.ast);
                    for import in imports {
                        let import = match import {
                            Ok(import) => import,
                            Err(error) => {
                                projects.issues.entry(module_ref.0).or_default().push(
                                    LoadProjectIssue::ScriptParseError {
                                        error,
                                        path: module_path.clone(),
                                    },
                                );
                                continue;
                            }
                        };
                        let import_specifier: Result<ImportSpecifier, _> = import.specifier.parse();
                        let Ok(import_specifier) = import_specifier;

                        match &import_specifier {
                            ImportSpecifier::Local(specifier) => {
                                let subpath = match specifier {
                                    LocalImportSpecifier::Relative(subpath) => {
                                        module_dir.join(RelativePath::new(&**subpath))
                                    }
                                    LocalImportSpecifier::ProjectRoot(subpath) => {
                                        RelativePath::new(&**subpath)
                                    }
                                };
                                let Ok(subpath) = subpath.normalized_subpath() else {
                                    projects.issues.entry(module_ref.0).or_default().push(
                                        LoadProjectIssue::ModuleImportEscapesProjectPath {
                                            import,
                                            path: module_path.clone(),
                                        },
                                    );
                                    continue;
                                };

                                let subpath = expand_module_subpath(subpath);
                                module_queue.push_back((
                                    subpath,
                                    ModuleReferrer::ModuleImport {
                                        referrer: module_ref,
                                        specifier: import_specifier,
                                        location: IssueLocation {
                                            path: module_path.clone(),
                                            range: Some(import.range),
                                        },
                                    },
                                ));
                            }
                            ImportSpecifier::External(specifier) => {
                                let location = IssueLocation {
                                    path: module_path.clone(),
                                    range: Some(import.range),
                                };
                                let issues = projects.issues.entry(project_ref.0).or_default();
                                let resolved = resolve_project(
                                    brioche,
                                    workspace,
                                    specifier,
                                    location.clone(),
                                    issues,
                                )
                                .await;

                                if let Some(resolved) = resolved {
                                    queue.push_back((
                                        resolved,
                                        ProjectReferrer::Project {
                                            referrer: project_ref,
                                            edge: ProjectEdge::ProjectDependency(specifier.clone()),
                                            location,
                                        },
                                    ));
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    let location = match module_referrer {
                        ModuleReferrer::ProjectRoot { .. } => match &referrer {
                            ProjectReferrer::Project { location, .. } => Some(location.clone()),
                            ProjectReferrer::TopLevel => None,
                        },
                        ModuleReferrer::ModuleImport { location, .. } => Some(location),
                    };
                    projects.issues.entry(project_ref.0).or_default().push(
                        LoadProjectIssue::LoadModuleError {
                            error: error.clone(),
                            path: module_path,
                            location,
                        },
                    );
                }
            }
        }

        let root_module_ref = &project_modules[&root_module_subpath];
        let root_module = &projects.modules[root_module_ref];

        let project_definition_value = root_module.as_ref().map_or_else(
            |_| Ok(None),
            |root_module| crate::script::parse::get_export_value(&root_module.ast, "project"),
        );
        let project_definition_value = match project_definition_value {
            Ok(value) => value,
            Err(error) => {
                projects.issues.entry(root_module_ref.0).or_default().push(
                    LoadProjectIssue::ScriptParseError {
                        error,
                        path: root_module_path.clone(),
                    },
                );
                None
            }
        };

        let project_definition_location = IssueLocation {
            path: root_module_path.clone(),
            range: project_definition_value.as_ref().map(|value| value.range),
        };
        let project_definition = project_definition_value.and_then(|value| {
            let project_definition: Result<ProjectDefinition, _> =
                serde_json::from_value(value.value);
            match project_definition {
                Ok(project_definition) => Some(project_definition),
                Err(error) => {
                    projects.issues.entry(root_module_ref.0).or_default().push(
                        LoadProjectIssue::InvalidProjectDefinition {
                            error_message: error.to_string(),
                            line: error.line(),
                            column: error.column(),
                            location: project_definition_location.clone(),
                        },
                    );
                    None
                }
            }
        });
        let project_definition = project_definition.unwrap_or_default();

        for (specifier, dep_definition) in &project_definition.dependencies {
            let issues = projects.issues.entry(project_ref.0).or_default();
            let resolved = match dep_definition {
                DependencyDefinition::Path { path } => {
                    let dep_path = project_path.join(RelativePath::new(path));
                    Some(ProjectSpecifier::Path(dep_path))
                }
                DependencyDefinition::Version(Version::Any) => {
                    resolve_project(
                        brioche,
                        workspace,
                        specifier,
                        project_definition_location.clone(),
                        issues,
                    )
                    .await
                }
            };

            if let Some(resolved) = resolved {
                queue.push_back((
                    resolved,
                    ProjectReferrer::Project {
                        referrer: project_ref,
                        edge: ProjectEdge::ProjectDependency(specifier.clone()),
                        location: project_definition_location.clone(),
                    },
                ));
            }
        }

        let project = Project {
            definition: project_definition,
            specifier,
        };
        projects.projects.insert(project_ref, project);
    }

    Ok(results)
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

pub async fn get_all_issues(brioche: &Brioche) -> Vec<LoadProjectIssue> {
    let projects = brioche.projects.read().await;

    projects
        .issues
        .values()
        .flat_map(|issues| issues.iter().cloned())
        .collect()
}

async fn find_workspace_root(
    path: &AbsolutePath,
) -> Result<Option<AbsolutePath>, LoadProjectError> {
    let mut current_path = path.clone();
    loop {
        let workspace_definition_path = current_path.join_one("brioche_workspace.toml");
        let workspace_definition_system_path = workspace_definition_path.to_system_path()?;
        let exists = tokio::fs::try_exists(&workspace_definition_system_path)
            .await
            .map_err(|error| LoadProjectError::IoError {
                error,
                path: workspace_definition_path.clone(),
            })?;
        if exists {
            return Ok(Some(current_path));
        }

        let Some(next_path) = current_path.parent() else {
            break;
        };
        current_path = next_path;
    }

    Ok(None)
}

async fn load_module(path: &Path) -> Result<Module, LoadModuleError> {
    let source = tokio::fs::read(path)
        .await
        .map_err(|error| LoadModuleError::IoError {
            error_message: error.to_string(),
        })?;
    let source = String::from_utf8(source)
        .map_err(|error| LoadModuleError::Utf8Error(error.utf8_error()))?;
    let ast = crate::script::parse::parse_script(&source);

    Ok(Module { ast })
}

async fn load_workspace(root: AbsolutePath) -> Result<Workspace, LoadWorkspaceError> {
    let workspace_definition_path = root
        .join_one("brioche_workspace.toml")
        .to_system_path()
        .unwrap_or_else(|error| {
            panic!("could not convert workspace root {root} to system path: {error}")
        });
    let source = tokio::fs::read(&workspace_definition_path)
        .await
        .map_err(|error| LoadWorkspaceError::IoError {
            error,
            path: workspace_definition_path.clone(),
        })?;
    let definition: WorkspaceDefinition =
        toml::from_slice(&source).map_err(|error| LoadWorkspaceError::ParseError {
            error,
            path: workspace_definition_path,
        })?;

    Ok(Workspace { root, definition })
}

fn expand_module_subpath(subpath: RelativePath) -> RelativePath {
    if subpath
        .filename()
        .is_some_and(|filename| filename.ends_with(b".bri"))
    {
        subpath
    } else if subpath.is_empty() {
        subpath.join_one("project.bri")
    } else {
        subpath.join_one("index.bri")
    }
}

async fn resolve_project(
    _brioche: &Brioche,
    workspace: Option<&Workspace>,
    specifier: &str,
    location: IssueLocation,
    issues: &mut Vec<LoadProjectIssue>,
) -> Option<ProjectSpecifier> {
    if let Some(workspace) = workspace
        && let Some(resolved) =
            resolve_project_from_workspace(workspace, specifier, location, issues).await
    {
        return Some(resolved);
    }

    todo!("resolve from registry: {specifier}");
}

async fn resolve_project_from_workspace(
    workspace: &Workspace,
    specifier: &str,
    location: IssueLocation,
    issues: &mut Vec<LoadProjectIssue>,
) -> Option<ProjectSpecifier> {
    for member in &workspace.definition.members {
        match member {
            WorkspaceMember::Path(parent, name) => {
                if name == specifier {
                    let member_path = workspace
                        .root
                        .join_subpath(parent.clone())
                        .expect("invalid workspace member subpath")
                        .join_one(name);
                    return Some(ProjectSpecifier::Path(member_path));
                }
            }
            WorkspaceMember::WildcardPath(parent) => {
                let member_path = workspace
                    .root
                    .join_subpath(parent.clone())
                    .expect("invalid workspace member subpath")
                    .join_one(specifier);
                let root_module_path = member_path.join_one("project.bri");
                let root_module_system_path = root_module_path.to_system_path();
                let root_module_system_path = match root_module_system_path {
                    Ok(path) => path,
                    Err(error) => {
                        issues.push(LoadProjectIssue::ToSystemPathError {
                            error,
                            path: root_module_path.into(),
                            location: location.clone(),
                        });
                        continue;
                    }
                };

                let exists = tokio::fs::try_exists(&root_module_system_path).await;
                match exists {
                    Ok(true) => return Some(ProjectSpecifier::Path(member_path)),
                    Ok(false) => {}
                    Err(error) => {
                        issues.push(LoadProjectIssue::IoError {
                            error_message: error.to_string(),
                            path: root_module_path,
                            location: location.clone(),
                        });
                    }
                }
            }
        }
    }

    todo!();
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LoadProjectIssue {
    #[error("{error}")]
    ScriptParseError {
        error: crate::script::parse::ScriptParseError,
        path: AbsolutePath,
    },

    #[error("{error}")]
    LoadModuleError {
        error: LoadModuleError,
        path: AbsolutePath,
        location: Option<IssueLocation>,
    },

    #[error("invalid project definition: {error_message}")]
    InvalidProjectDefinition {
        error_message: String,
        line: usize,
        column: usize,
        location: IssueLocation,
    },

    #[error("IO error at {path}: {error_message}")]
    IoError {
        error_message: String,
        path: AbsolutePath,
        location: IssueLocation,
    },

    #[error("invalid path '{path}': {error}")]
    ToSystemPathError {
        error: crate::path::ToSystemPathError,
        path: AnyPath,
        location: IssueLocation,
    },

    #[error("module import '{}' escapes project path", import.specifier)]
    ModuleImportEscapesProjectPath {
        import: crate::script::parse::ScriptImport,
        path: AbsolutePath,
    },
}

impl LoadProjectIssue {
    #[must_use]
    pub fn location(&self) -> Option<IssueLocation> {
        match self {
            Self::InvalidProjectDefinition { location, .. }
            | Self::IoError { location, .. }
            | Self::ToSystemPathError { location, .. } => Some(location.clone()),
            Self::LoadModuleError { location, .. } => location.clone(),
            Self::ScriptParseError { error, path } => Some(IssueLocation {
                path: path.clone(),
                range: Some(error.range()),
            }),
            Self::ModuleImportEscapesProjectPath { import, path } => Some(IssueLocation {
                path: path.clone(),
                range: Some(import.range),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IssueLocation {
    pub path: AbsolutePath,
    pub range: Option<crate::script::parse::TextRange>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadProjectError {
    #[error(transparent)]
    ToSystemPathError(#[from] crate::path::ToSystemPathError),

    #[error("IO error at {path}: {error}")]
    IoError {
        #[source]
        error: std::io::Error,
        path: AbsolutePath,
    },

    #[error(transparent)]
    CanonicalSystemPathError(#[from] crate::path::CanonicalSystemPathError),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LoadModuleError {
    #[error("IO error: {error_message}")]
    IoError { error_message: String },
    #[error(transparent)]
    Utf8Error(std::str::Utf8Error),
}

#[derive(Debug, thiserror::Error)]
enum LoadWorkspaceError {
    #[error("failed to load workspace at {}: {error}", path.display())]
    IoError {
        #[source]
        error: std::io::Error,
        path: std::path::PathBuf,
    },
    #[error("failed to parse workspace definition at {}: {error}", path.display())]
    ParseError {
        #[source]
        error: toml::de::Error,
        path: std::path::PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceMemberParseError {
    #[error("invalid glob pattern in workspace member path")]
    InvalidGlobPattern,

    #[error(transparent)]
    SubpathError(#[from] crate::path::SubpathError),
}
