use std::{
    collections::{HashMap, VecDeque},
    path::Path,
};

use petgraph::stable_graph::NodeIndex;

use crate::{
    Brioche,
    path::RelativePath,
    script::specifier::{ImportSpecifier, LocalImportSpecifier},
};

#[derive(Default)]
pub struct Projects {
    graph: petgraph::stable_graph::StableDiGraph<ProjectNode, ProjectEdge>,
    projects: HashMap<ProjectRef, Project>,
    modules: HashMap<ModuleRef, Result<Module, LoadModuleError>>,
    projects_by_specifier: HashMap<ProjectSpecifier, ProjectRef>,
    issues: HashMap<NodeIndex, Vec<LoadProjectIssue>>,
}

pub(crate) enum ProjectNode {
    Project,
    Module,
}

pub(crate) enum ProjectEdge {
    ProjectDependency(String),
    ProjectRootModule,
    ModuleImport(ImportSpecifier),
}

pub(crate) struct Project {
    pub definition: ProjectDefinition,
    pub specifier: ProjectSpecifier,
}

pub(crate) struct Module {
    ast: crate::script::parse::ScriptAst,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectDefinition {
    pub name: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub dependencies: HashMap<String, DependencyDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum DependencyDefinition {
    Path { path: String },
    Version(Version),
}

#[derive(
    Debug, Clone, PartialEq, Eq, serde_with::DeserializeFromStr, serde_with::SerializeDisplay,
)]
pub(crate) enum Version {
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
pub struct ProjectHash(crate::hash::Hash);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectRef(NodeIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ModuleRef(NodeIndex);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectSpecifier {
    Path(crate::path::AbsolutePath),
    Hash(ProjectHash),
}

pub async fn load_projects(
    brioche: &Brioche,
    specifiers: impl IntoIterator<Item = ProjectSpecifier>,
) -> Result<HashMap<ProjectSpecifier, ProjectRef>, LoadProjectError> {
    let mut queue = specifiers
        .into_iter()
        .map(|specifier| (specifier, true))
        .collect::<VecDeque<_>>();
    let mut projects = brioche.projects.write().await;
    let projects = &mut *projects;
    let mut results = HashMap::new();

    while let Some((specifier, is_result)) = queue.pop_front() {
        if let Some(project) = projects.projects_by_specifier.get(&specifier) {
            if is_result {
                results.insert(specifier, *project);
            }

            continue;
        }

        let project_path = match &specifier {
            ProjectSpecifier::Path(path) => path,
            ProjectSpecifier::Hash(_project_hash) => {
                todo!("load project by hash")
            }
        };

        let project_ref = projects.graph.add_node(ProjectNode::Project);
        let project_ref = ProjectRef(project_ref);

        let root_module_path = RelativePath::one("project.bri");

        let mut module_queue = VecDeque::from_iter([(
            root_module_path.clone(),
            ProjectEdge::ProjectRootModule,
            project_ref.0,
        )]);
        let mut project_modules = HashMap::<RelativePath, ModuleRef>::new();

        while let Some((module_subpath, edge, referrer)) = module_queue.pop_front() {
            if let Some(module_ref) = project_modules.get(&module_subpath) {
                projects.graph.add_edge(referrer, module_ref.0, edge);
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
            projects.graph.add_edge(referrer, module_ref.0, edge);
            project_modules.insert(module_subpath, module_ref);

            let module_system_path = module_path.to_system_path()?;
            let module = load_module(&module_system_path).await;

            let module_entry = projects.modules.entry(module_ref).insert_entry(module);
            let module = module_entry.get();
            if let Ok(module) = &module {
                let imports = crate::script::parse::find_imports(&module.ast);
                for import in imports {
                    let import = match import {
                        Ok(import) => import,
                        Err(error) => {
                            projects
                                .issues
                                .entry(module_ref.0)
                                .or_default()
                                .push(LoadProjectIssue::ScriptParseError(error));
                            continue;
                        }
                    };
                    let import_specifier: Result<ImportSpecifier, _> = import.specifier.parse();
                    let Ok(import_specifier) = import_specifier;

                    match &import_specifier {
                        ImportSpecifier::Local(specifier) => {
                            let subpath = match specifier {
                                LocalImportSpecifier::Relative(subpath) => {
                                    module_dir.join(RelativePath::from(&**subpath))
                                }
                                LocalImportSpecifier::ProjectRoot(subpath) => {
                                    RelativePath::from(&**subpath)
                                }
                            };
                            let Ok(subpath) = subpath.normalized_subpath() else {
                                projects.issues.entry(module_ref.0).or_default().push(
                                    LoadProjectIssue::ModuleImportEscapesProjectPath { import },
                                );
                                continue;
                            };

                            if subpath
                                .filename()
                                .is_some_and(|filename| filename.ends_with(b".bri"))
                            {
                                module_queue.push_back((
                                    subpath,
                                    ProjectEdge::ModuleImport(import_specifier),
                                    module_ref.0,
                                ));
                            } else if subpath.is_empty() {
                                module_queue.push_back((
                                    subpath.join_one("project.bri"),
                                    ProjectEdge::ModuleImport(import_specifier),
                                    module_ref.0,
                                ));
                            } else {
                                module_queue.push_back((
                                    subpath.join_one("index.bri"),
                                    ProjectEdge::ModuleImport(import_specifier),
                                    module_ref.0,
                                ));
                            }
                        }
                        ImportSpecifier::External(_) => todo!("external import"),
                    }
                }
            }
        }

        let root_module_ref = &project_modules[&root_module_path];
        let root_module = &projects.modules[root_module_ref];

        let project_definition_value = root_module.as_ref().map_or_else(
            |_| Ok(None),
            |root_module| crate::script::parse::get_export_value(&root_module.ast, "project"),
        );
        let project_definition_value = match project_definition_value {
            Ok(value) => value,
            Err(error) => {
                projects
                    .issues
                    .entry(root_module_ref.0)
                    .or_default()
                    .push(LoadProjectIssue::ScriptParseError(error));
                None
            }
        };
        let project_definition = project_definition_value.and_then(|value| {
            let project_definition: Result<ProjectDefinition, _> =
                serde_json::from_value(value.value);
            match project_definition {
                Ok(project_definition) => Some(project_definition),
                Err(error) => {
                    projects.issues.entry(root_module_ref.0).or_default().push(
                        LoadProjectIssue::InvalidProjectDefinition {
                            error,
                            range: value.range,
                        },
                    );
                    None
                }
            }
        });
        let project_definition = project_definition.unwrap_or_default();

        let project = Project {
            definition: project_definition,
            specifier,
        };
        projects.projects.insert(project_ref, project);
    }

    Ok(results)
}

async fn load_module(path: &Path) -> Result<Module, LoadModuleError> {
    let source = tokio::fs::read(path)
        .await
        .map_err(|error| LoadModuleError::IoError {
            error,
            path: path.to_path_buf(),
        })?;
    let source = String::from_utf8(source).map_err(|error| LoadModuleError::FileUtf8Error {
        error,
        path: path.to_path_buf(),
    })?;
    let ast = crate::script::parse::parse_script(&source);

    Ok(Module { ast })
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LoadProjectIssue {
    #[error(transparent)]
    ScriptParseError(crate::script::parse::ScriptParseError),

    #[error("invalid project definition: {error}")]
    InvalidProjectDefinition {
        error: serde_json::Error,
        range: crate::script::parse::TextRange,
    },

    #[error("module import '{}' escapes project path", import.specifier)]
    ModuleImportEscapesProjectPath {
        import: crate::script::parse::ScriptImport,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum LoadProjectError {
    #[error(transparent)]
    ToSystemPathError(#[from] crate::path::ToSystemPathError),
}

#[derive(Debug, thiserror::Error)]
enum LoadModuleError {
    #[error("failed to load module at {}: {error}", path.display())]
    IoError {
        #[source]
        error: std::io::Error,
        path: std::path::PathBuf,
    },
    #[error("module at {} is not UTF-8: {error}", path.display())]
    FileUtf8Error {
        #[source]
        error: std::string::FromUtf8Error,
        path: std::path::PathBuf,
    },
}
