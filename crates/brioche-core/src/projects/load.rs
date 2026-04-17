use std::collections::{HashMap, VecDeque};

use crate::{
    Brioche,
    path::{AbsolutePath, RelativePath},
    projects::{
        DependencyDefinition, Module, ModuleRef, ModuleReferrer, Project, ProjectDefinition,
        ProjectEdge, ProjectIssue, ProjectIssueLocation, ProjectNode, ProjectRef, ProjectReferrer,
        ProjectSpecifier, Version, Workspace, WorkspaceDefinition, WorkspaceMember, WorkspaceRef,
    },
    script::specifier::{ImportSpecifier, LocalImportSpecifier},
};

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

        let mut project_definition = ProjectDefinition::default();
        let mut project_modules = HashMap::<RelativePath, ModuleRef>::new();
        let mut external_deps = HashMap::<String, Option<ProjectSpecifier>>::new();

        let mut module_queue = VecDeque::from_iter([(
            root_module_subpath.clone(),
            ModuleReferrer::ProjectRoot { project_ref },
        )]);
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
            project_modules.insert(module_subpath.clone(), module_ref);

            let module_system_path = module_path.to_system_path()?;
            let module_ast = load_module_ast(&module_system_path).await;
            let module = Module {
                ast: module_ast,
                project: project_ref,
                subpath: module_subpath,
            };

            let module_entry = projects.modules.entry(module_ref).insert_entry(module);
            let module = module_entry.get();

            match module {
                Module {
                    ast: Ok(module_ast),
                    ..
                } => {
                    if let ModuleReferrer::ProjectRoot { .. } = module_referrer {
                        let project_definition_value =
                            crate::script::parse::get_export_value(module_ast, "project");
                        let project_definition_value = match project_definition_value {
                            Ok(value) => value,
                            Err(error) => {
                                projects.issues.entry(module_ref.0).or_default().push(
                                    ProjectIssue::ScriptParseError {
                                        error,
                                        path: root_module_path.clone(),
                                    },
                                );
                                None
                            }
                        };

                        let project_definition_location = ProjectIssueLocation {
                            path: root_module_path.clone(),
                            range: project_definition_value.as_ref().map(|value| value.range),
                        };
                        project_definition = project_definition_value
                            .and_then(|value| {
                                let project_definition: Result<ProjectDefinition, _> =
                                    serde_json::from_value(value.value);
                                match project_definition {
                                    Ok(project_definition) => Some(project_definition),
                                    Err(error) => {
                                        projects.issues.entry(module_ref.0).or_default().push(
                                            ProjectIssue::InvalidProjectDefinition {
                                                error_message: error.to_string(),
                                                line: error.line(),
                                                column: error.column(),
                                                location: project_definition_location.clone(),
                                            },
                                        );
                                        None
                                    }
                                }
                            })
                            .unwrap_or_default();
                    }

                    let imports = crate::script::parse::find_imports(module_ast);
                    for import in imports {
                        let import = match import {
                            Ok(import) => import,
                            Err(error) => {
                                projects.issues.entry(module_ref.0).or_default().push(
                                    ProjectIssue::ScriptParseError {
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
                                        ProjectIssue::ModuleImportEscapesProjectPath {
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
                                        location: ProjectIssueLocation {
                                            path: module_path.clone(),
                                            range: Some(import.range),
                                        },
                                    },
                                ));
                            }
                            ImportSpecifier::External(specifier) => {
                                let location = ProjectIssueLocation {
                                    path: module_path.clone(),
                                    range: Some(import.range),
                                };
                                let issues = projects.issues.entry(project_ref.0).or_default();
                                let resolved = resolve_project(
                                    brioche,
                                    &mut ResolveProjectContext {
                                        project_path,
                                        project_definition: &project_definition,
                                        workspace,
                                        external_deps: &mut external_deps,
                                        issues,
                                    },
                                    specifier,
                                    location.clone(),
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
                Module {
                    ast: Err(error), ..
                } => {
                    let location = match module_referrer {
                        ModuleReferrer::ProjectRoot { .. } => match &referrer {
                            ProjectReferrer::Project { location, .. } => Some(location.clone()),
                            ProjectReferrer::TopLevel => None,
                        },
                        ModuleReferrer::ModuleImport { location, .. } => Some(location),
                    };
                    projects.issues.entry(project_ref.0).or_default().push(
                        ProjectIssue::LoadModuleError {
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

        let project_definition_value = root_module.ast.as_ref().map_or_else(
            |_| Ok(None),
            |ast| crate::script::parse::get_export_value(ast, "project"),
        );
        let project_definition_value = match project_definition_value {
            Ok(value) => value,
            Err(error) => {
                projects.issues.entry(root_module_ref.0).or_default().push(
                    ProjectIssue::ScriptParseError {
                        error,
                        path: root_module_path.clone(),
                    },
                );
                None
            }
        };

        let project_definition_location = ProjectIssueLocation {
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
                        ProjectIssue::InvalidProjectDefinition {
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

        for specifier in project_definition.dependencies.keys() {
            let issues = projects.issues.entry(project_ref.0).or_default();
            let resolved = resolve_project(
                brioche,
                &mut ResolveProjectContext {
                    project_path,
                    project_definition: &project_definition,
                    workspace,
                    external_deps: &mut external_deps,
                    issues,
                },
                specifier,
                project_definition_location.clone(),
            )
            .await;

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

        projects
            .modules_by_project
            .insert(project_ref, project_modules);
    }

    Ok(results)
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
pub(super) enum LoadWorkspaceError {
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

async fn load_module_ast(
    path: &std::path::Path,
) -> Result<crate::script::parse::ScriptAst, LoadModuleError> {
    let source = tokio::fs::read(path)
        .await
        .map_err(|error| LoadModuleError::IoError {
            error_message: error.to_string(),
        })?;
    let source = String::from_utf8(source)
        .map_err(|error| LoadModuleError::Utf8Error(error.utf8_error()))?;
    Ok(crate::script::parse::parse_script(source))
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

struct ResolveProjectContext<'a> {
    project_path: &'a AbsolutePath,
    project_definition: &'a ProjectDefinition,
    workspace: Option<&'a Workspace>,
    external_deps: &'a mut HashMap<String, Option<ProjectSpecifier>>,
    issues: &'a mut Vec<ProjectIssue>,
}

async fn resolve_project(
    _brioche: &Brioche,
    ctx: &mut ResolveProjectContext<'_>,
    specifier: &str,
    location: ProjectIssueLocation,
) -> Option<ProjectSpecifier> {
    if let Some(resolved) = ctx
        .external_deps
        .get(specifier)
        .and_then(|resolved| resolved.as_ref())
    {
        return Some(resolved.clone());
    }

    let _version = match ctx.project_definition.dependencies.get(specifier) {
        Some(DependencyDefinition::Path { path }) => {
            let dep_path = ctx.project_path.join(RelativePath::new(path));
            let resolved = Some(ProjectSpecifier::Path(dep_path));
            ctx.external_deps
                .insert(specifier.to_string(), resolved.clone());
            return resolved;
        }
        Some(DependencyDefinition::Version(version)) => version.clone(),
        None => Version::Any,
    };

    if let Some(resolved) = resolve_project_from_workspace(ctx, specifier, location).await {
        ctx.external_deps
            .insert(specifier.to_string(), Some(resolved.clone()));
        return Some(resolved);
    }

    todo!("resolve from registry: {specifier}");
}

async fn resolve_project_from_workspace(
    ctx: &mut ResolveProjectContext<'_>,
    specifier: &str,
    location: ProjectIssueLocation,
) -> Option<ProjectSpecifier> {
    let workspace = ctx.workspace?;
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
                        ctx.issues.push(ProjectIssue::ToSystemPathError {
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
                        ctx.issues.push(ProjectIssue::IoError {
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
