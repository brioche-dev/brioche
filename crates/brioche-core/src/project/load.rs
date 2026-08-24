use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
};

use futures::TryFutureExt as _;
use petgraph::visit::EdgeRef as _;
use tokio::io::AsyncReadExt as _;

use crate::{
    BriocheResources, BriocheState,
    path::{AbsolutePath, RelativePath},
    project::{
        DependencyDefinition, Lockfile, LockfileState, Module, ModuleRef, ModuleReferrer, Project,
        ProjectDefinition, ProjectEdge, ProjectIssue, ProjectIssueLocation, ProjectNode,
        ProjectRef, ProjectReferrer, ProjectSpecifier, SharedStatic, Static, StaticQuery,
        StaticRef, UnresolvedStatic, Version, Workspace, WorkspaceDefinition, WorkspaceMember,
        WorkspaceRef, hash::ProjectHash,
    },
    recipe::RecipeHash,
    reporter::job::JobContext,
    script::specifier::{ImportSpecifier, LocalImportSpecifier},
};

#[tracing::instrument(skip_all)]
pub async fn load_projects(
    brioche: &mut BriocheState,
    specifiers: impl IntoIterator<Item = ProjectSpecifier>,
) -> Result<HashMap<ProjectSpecifier, ProjectRef>, LoadProjectError> {
    let mut queue = specifiers
        .into_iter()
        .map(|specifier| (specifier, ProjectReferrer::TopLevel))
        .collect::<VecDeque<_>>();
    let mut project_hashes_to_validate = HashMap::new();
    let mut results = HashMap::new();

    let mut shared_statics = HashMap::<SharedStatic, StaticRef>::new();

    while let Some((specifier, referrer)) = queue.pop_front() {
        let project = brioche
            .projects
            .projects_by_specifier
            .get(&specifier)
            .copied();
        if let Some(project) = project {
            tracing::trace!(?specifier, ?referrer, "project already loaded");

            match referrer {
                ProjectReferrer::TopLevel => {
                    results.insert(specifier, project);
                }
                ProjectReferrer::Project { referrer, edge, .. } => {
                    brioche
                        .projects
                        .graph
                        .update_edge(referrer.0, project.0, *edge);
                }
            }

            continue;
        }

        let project_ref = brioche.projects.graph.add_node(ProjectNode::Project);
        let project_ref = ProjectRef(project_ref);

        brioche
            .projects
            .projects_by_specifier
            .insert(specifier.clone(), project_ref);

        tracing::debug!(?specifier, ?referrer, ?project_ref, "loading project");

        let (project_path, workspace_root) = match &specifier {
            ProjectSpecifier::Path(path) => {
                let workspace_root = find_workspace_root_absolute(path).await?;

                tracing::trace!(?path, ?workspace_root, "searched for workspace root");

                (path.clone(), workspace_root)
            }
            ProjectSpecifier::Hash(project_hash) => {
                project_hashes_to_validate.insert(project_ref, *project_hash);

                match load_project_by_hash(brioche, *project_hash).await {
                    Ok((path, workspace_root)) => {
                        brioche
                            .projects
                            .projects_by_specifier
                            .insert(ProjectSpecifier::Path(path.clone()), project_ref);

                        (path, workspace_root)
                    }
                    Err(error) => {
                        let location = match referrer {
                            ProjectReferrer::TopLevel => ProjectIssueLocation {
                                source: project_ref.into(),
                                range: None,
                            },
                            ProjectReferrer::Project {
                                module_referrer: (module_ref, _),
                                ..
                            } => ProjectIssueLocation {
                                source: module_ref.into(),
                                range: None,
                            },
                        };
                        brioche
                            .projects
                            .issues
                            .entry(project_ref.0)
                            .or_default()
                            .push(ProjectIssue::LoadProjectByHashError {
                                error,
                                project_hash: *project_hash,
                                location,
                            });
                        continue;
                    }
                }
            }
        };
        let workspace_membership = workspace_root.map(|workspace_root| {
            let subpath = crate::path::relative_path_between(&workspace_root, &project_path).unwrap_or_else(|error| panic!("expected project {project_path} to be within the workspace {workspace_root}, but could not get relative path: {error}"));
            (workspace_root, subpath)
        });

        brioche
            .projects
            .local_project_paths
            .insert(project_ref, project_path.clone());

        match &referrer {
            ProjectReferrer::TopLevel => {
                results.insert(specifier.clone(), project_ref);
            }
            ProjectReferrer::Project { referrer, edge, .. } => {
                brioche
                    .projects
                    .graph
                    .update_edge(referrer.0, project_ref.0, (**edge).clone());
            }
        }

        let workspace_ref = if let Some((workspace_root, workspace_subpath)) = workspace_membership
        {
            let state = &mut *brioche;
            match state.projects.workspaces_by_path.entry(workspace_root) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    state.projects.graph.update_edge(
                        entry.get().0,
                        project_ref.0,
                        ProjectEdge::ProjectWithinWorkspace(workspace_subpath),
                    );

                    Some(*entry.get())
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let workspace_ref = state.projects.graph.add_node(ProjectNode::Workspace);
                    let workspace_ref = WorkspaceRef(workspace_ref);

                    state.projects.graph.update_edge(
                        workspace_ref.0,
                        project_ref.0,
                        ProjectEdge::ProjectWithinWorkspace(workspace_subpath),
                    );

                    let workspace = load_workspace(entry.key().clone()).await;

                    tracing::trace!(workspace = ?workspace.as_ref().map(|_| ()), "loaded new workspace");

                    entry.insert(workspace_ref);
                    state.projects.workspaces.insert(workspace_ref, workspace);

                    Some(workspace_ref)
                }
            }
        } else {
            None
        };

        let lockfile_path = project_path.join_one("brioche.lock");
        let lockfile_system_path = lockfile_path.to_system_path()?;
        let lockfile_content = tokio::fs::read(&lockfile_system_path).await;
        let lockfile_content = match lockfile_content {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(LoadProjectError::IoError {
                    error,
                    reason: format!("failed to read lockfile '{lockfile_path}'").into(),
                });
            }
        };
        let lockfile_with_content = lockfile_content.as_deref().map_or_else(
            || Err(LockfileIssue::NotFound),
            |content| {
                let content_str = std::str::from_utf8(content).map_err(LockfileIssue::Utf8Error)?;
                let lockfile: Lockfile =
                    serde_json::from_str(content_str).map_err(LockfileIssue::DeserializeError)?;
                Ok((lockfile, content))
            },
        );
        let lockfile = lockfile_with_content.as_ref().map(|(lockfile, _)| lockfile);
        let mut new_lockfile = Lockfile::default();

        let root_module_subpath = RelativePath::one("project.bri");

        let mut project_definition = ProjectDefinition::default();
        let mut project_modules = HashMap::<RelativePath, ModuleRef>::new();
        let mut external_deps = HashMap::<String, Option<ProjectSpecifier>>::new();
        let mut module_asts = HashMap::<ModuleRef, crate::script::parse::ScriptAst>::new();

        let module_referrer = match referrer {
            ProjectReferrer::TopLevel => None,
            ProjectReferrer::Project {
                module_referrer, ..
            } => Some(module_referrer),
        };
        let mut module_queue = VecDeque::from_iter([(
            root_module_subpath.clone(),
            ModuleReferrer::ProjectRoot {
                project_ref,
                referrer: module_referrer,
            },
        )]);
        while let Some((module_subpath, module_referrer)) = module_queue.pop_front() {
            if let Some(module_ref) = project_modules.get(&module_subpath) {
                brioche.projects.graph.update_edge(
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
                .join_subpath(&module_subpath)
                .unwrap_or_else(|error| {
                    panic!("module subpath {module_subpath} escapes project path {project_path}: {error}")
                });

            let module_ref = brioche.projects.graph.add_node(ProjectNode::Module);
            let module_ref = ModuleRef(module_ref);
            brioche.projects.graph.update_edge(
                module_referrer.node_index(),
                module_ref.0,
                module_referrer.edge(),
            );
            project_modules.insert(module_subpath.clone(), module_ref);
            brioche
                .projects
                .project_by_module
                .insert(module_ref, (project_ref, module_subpath.clone()));
            brioche
                .projects
                .modules_by_path
                .insert(module_path.clone(), module_ref);

            tracing::trace!(?project_ref, ?module_subpath, ?module_ref, "loading module");

            let module_system_path = module_path.to_system_path()?;
            let module_source = load_module_source(&module_system_path).await;
            let module = Module {
                project: project_ref,
                source: module_source,
                subpath: module_subpath,
            };

            brioche.projects.modules.insert(module_ref, module);

            let module_ast = brioche.projects.modules[&module_ref]
                .source
                .as_deref()
                .map(crate::script::parse::parse_script)
                .map_err(Clone::clone);
            let module_ast_entry =
                module_ast.map(|ast| module_asts.entry(module_ref).insert_entry(ast));
            let module_ast = module_ast_entry
                .as_ref()
                .map(std::collections::hash_map::OccupiedEntry::get);

            match module_ast {
                Ok(module_ast) => {
                    let mut env = HashMap::new();

                    if let ModuleReferrer::ProjectRoot { .. } = module_referrer {
                        let project_definition_value =
                            crate::script::parse::get_export_value(module_ast, "project");
                        let project_definition_value = match project_definition_value {
                            Ok(value) => value,
                            Err(error) => {
                                brioche
                                    .projects
                                    .issues
                                    .entry(module_ref.0)
                                    .or_default()
                                    .push(ProjectIssue::ScriptParseError { error, module_ref });
                                None
                            }
                        };

                        // Insert the `project` export in the env, so it can
                        // be referenced when resolving statics
                        if let Some(project) = &project_definition_value {
                            env.insert("project".to_string(), project.value.clone());
                        }

                        let project_definition_location = ProjectIssueLocation {
                            source: module_ref.into(),
                            range: project_definition_value.as_ref().map(|value| value.range),
                        };
                        project_definition = project_definition_value
                            .and_then(|value| {
                                let project_definition: Result<ProjectDefinition, _> =
                                    serde_json::from_value(value.value);
                                match project_definition {
                                    Ok(project_definition) => Some(project_definition),
                                    Err(error) => {
                                        brioche
                                            .projects
                                            .issues
                                            .entry(module_ref.0)
                                            .or_default()
                                            .push(ProjectIssue::InvalidProjectDefinition {
                                                error_message: error.to_string(),
                                                line: error.line(),
                                                column: error.column(),
                                                location: project_definition_location,
                                            });
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
                                brioche
                                    .projects
                                    .issues
                                    .entry(module_ref.0)
                                    .or_default()
                                    .push(ProjectIssue::ScriptParseError { error, module_ref });
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
                                    let range = Some(import.range);
                                    brioche
                                        .projects
                                        .issues
                                        .entry(module_ref.0)
                                        .or_default()
                                        .push(ProjectIssue::ModuleImportEscapesProjectPath {
                                            import,
                                            location: ProjectIssueLocation {
                                                source: module_ref.into(),
                                                range,
                                            },
                                        });
                                    continue;
                                };

                                let subpath = expand_module_subpath(subpath);
                                module_queue.push_back((
                                    subpath,
                                    ModuleReferrer::ModuleImport {
                                        referrer: (module_ref, import.range),
                                        specifier: import_specifier,
                                    },
                                ));
                            }
                            ImportSpecifier::External(specifier) => {
                                let issues =
                                    brioche.projects.issues.entry(project_ref.0).or_default();
                                let workspace = workspace_ref.and_then(|workspace_ref| {
                                    brioche.projects.workspaces[&workspace_ref].as_ref().ok()
                                });
                                let resolved_dep = resolve_project_dependency(
                                    &brioche.resources,
                                    &mut ResolveProjectDependencyContext {
                                        project_path: &project_path,
                                        project_definition: &project_definition,
                                        workspace,
                                        external_deps: &mut external_deps,
                                        issues,
                                        lockfile: lockfile.ok(),
                                        new_lockfile: &mut new_lockfile,
                                    },
                                    specifier,
                                    ProjectIssueLocation {
                                        source: module_ref.into(),
                                        range: Some(import.range),
                                    },
                                )
                                .await;

                                if let Some(resolved_dep) = resolved_dep {
                                    queue.push_back((
                                        resolved_dep,
                                        ProjectReferrer::Project {
                                            referrer: project_ref,
                                            edge: Box::new(ProjectEdge::ProjectDependency(
                                                specifier.clone(),
                                            )),
                                            module_referrer: (module_ref, Some(import.range)),
                                        },
                                    ));
                                }
                            }
                        }
                    }

                    let static_queries = crate::script::parse::find_statics(module_ast, &env);
                    for query in static_queries {
                        let query = match query {
                            Ok(query) => query,
                            Err(error) => {
                                brioche
                                    .projects
                                    .issues
                                    .entry(module_ref.0)
                                    .or_default()
                                    .push(ProjectIssue::ScriptParseError { error, module_ref });
                                continue;
                            }
                        };
                        let static_ = prepare_static(query.query.clone(), lockfile.ok());

                        let static_ref = match static_ {
                            PartialStatic::Shared(static_) => {
                                *shared_statics.entry(static_).or_insert_with_key(|static_| {
                                    let static_ref = StaticRef(
                                        brioche.projects.graph.add_node(ProjectNode::Static),
                                    );
                                    brioche
                                        .projects
                                        .statics
                                        .insert(static_ref, static_.clone().into());
                                    static_ref
                                })
                            }
                            PartialStatic::Unique(static_) => {
                                let static_ref =
                                    StaticRef(brioche.projects.graph.add_node(ProjectNode::Static));
                                brioche.projects.statics.insert(static_ref, static_);
                                static_ref
                            }
                            PartialStatic::Unresolved(static_) => {
                                let state = &mut *brioche;
                                let location = ProjectIssueLocation {
                                    source: module_ref.into(),
                                    range: Some(query.range),
                                };
                                let static_ref_entry = state
                                    .projects
                                    .static_ref_by_unresolved_static
                                    .entry(static_.clone());
                                let static_ref = match static_ref_entry {
                                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                                        let (static_ref, locations) = entry.get_mut();
                                        locations.push(location);
                                        *static_ref
                                    }
                                    std::collections::hash_map::Entry::Vacant(entry) => {
                                        let static_ref = StaticRef(
                                            state
                                                .projects
                                                .graph
                                                .add_node(ProjectNode::UnresolvedStatic),
                                        );
                                        entry.insert((static_ref, vec![location]));
                                        static_ref
                                    }
                                };
                                brioche
                                    .projects
                                    .unresolved_statics
                                    .insert(static_ref, static_);
                                static_ref
                            }
                        };

                        brioche.projects.graph.add_edge(
                            module_ref.0,
                            static_ref.0,
                            ProjectEdge::ModuleStatic(query),
                        );
                    }
                }
                Err(error) => {
                    brioche
                        .projects
                        .issues
                        .entry(project_ref.0)
                        .or_default()
                        .push(ProjectIssue::LoadModuleError {
                            error: (*error).clone(),
                            module_ref,
                            referrer: module_referrer.referrer_and_range(),
                        });
                }
            }
        }

        let root_module_ref = project_modules[&root_module_subpath];
        let root_module_ast = module_asts.get(&root_module_ref);

        let project_definition_value = root_module_ast.map_or_else(
            || Ok(None),
            |ast| crate::script::parse::get_export_value(ast, "project"),
        );
        let project_definition_value = match project_definition_value {
            Ok(value) => value,
            Err(error) => {
                brioche
                    .projects
                    .issues
                    .entry(root_module_ref.0)
                    .or_default()
                    .push(ProjectIssue::ScriptParseError {
                        error,
                        module_ref: root_module_ref,
                    });
                None
            }
        };

        let project_definition_range = project_definition_value.as_ref().map(|value| value.range);
        let project_definition_location = ProjectIssueLocation {
            source: root_module_ref.into(),
            range: project_definition_range,
        };
        let project_definition = project_definition_value.and_then(|value| {
            let project_definition: Result<ProjectDefinition, _> =
                serde_json::from_value(value.value);
            match project_definition {
                Ok(project_definition) => Some(project_definition),
                Err(error) => {
                    brioche
                        .projects
                        .issues
                        .entry(root_module_ref.0)
                        .or_default()
                        .push(ProjectIssue::InvalidProjectDefinition {
                            error_message: error.to_string(),
                            line: error.line(),
                            column: error.column(),
                            location: project_definition_location,
                        });
                    None
                }
            }
        });
        let project_definition = project_definition.unwrap_or_default();

        for specifier in project_definition.dependencies.keys() {
            let issues = brioche.projects.issues.entry(project_ref.0).or_default();
            let workspace = workspace_ref.and_then(|workspace_ref| {
                brioche.projects.workspaces[&workspace_ref].as_ref().ok()
            });
            let resolved_dep = resolve_project_dependency(
                &brioche.resources,
                &mut ResolveProjectDependencyContext {
                    project_path: &project_path,
                    project_definition: &project_definition,
                    workspace,
                    external_deps: &mut external_deps,
                    issues,
                    lockfile: lockfile.ok(),
                    new_lockfile: &mut new_lockfile,
                },
                specifier,
                project_definition_location,
            )
            .await;

            if let Some(resolved_dep) = resolved_dep {
                queue.push_back((
                    resolved_dep,
                    ProjectReferrer::Project {
                        referrer: project_ref,
                        edge: Box::new(ProjectEdge::ProjectDependency(specifier.clone())),
                        module_referrer: (root_module_ref, project_definition_range),
                    },
                ));
            }
        }

        let project = Project {
            definition: project_definition,
            specifier,
            lockfile_state: LockfileState::new(lockfile_with_content.ok(), new_lockfile),
        };
        brioche.projects.projects.insert(project_ref, project);

        brioche
            .projects
            .modules_by_project
            .insert(project_ref, project_modules);
    }

    // Skip project hash validation for any projects that already had
    // other issues
    project_hashes_to_validate.retain(|project_ref, _| {
        brioche
            .projects
            .issues
            .get(&project_ref.0)
            .is_none_or(Vec::is_empty)
    });

    if !project_hashes_to_validate.is_empty() {
        let project_groups = crate::project::hash::group_project_nodes(
            &brioche.projects,
            project_hashes_to_validate.keys().copied(),
        );

        let mut project_hashes = HashMap::new();
        let hash_result = crate::project::hash::hash_projects_inner(
            brioche,
            &project_groups,
            &mut project_hashes,
            None,
        )
        .await
        .map_err(Arc::new);

        let mut added_failed_to_hash_issue = false;
        for (project_ref, expected_hash) in project_hashes_to_validate {
            if let Some(actual_hash) = project_hashes.get(&project_ref) {
                if expected_hash != *actual_hash {
                    brioche
                        .projects
                        .issues
                        .entry(project_ref.0)
                        .or_default()
                        .push(ProjectIssue::ProjectHashMismatch {
                            expected_hash,
                            actual_hash: *actual_hash,
                        });
                }
            } else if let Err(error) = &hash_result {
                brioche
                    .projects
                    .issues
                    .entry(project_ref.0)
                    .or_default()
                    .push(ProjectIssue::FailedToValidateHash {
                        expected_hash,
                        error: error.clone(),
                    });
                added_failed_to_hash_issue = true;
            } else {
                unreachable!("expected either a project hash or an error while hashing");
            }
        }

        if let Err(error) = hash_result
            && !added_failed_to_hash_issue
        {
            tracing::warn!("encountered unreported error while hashing projects: {error:?}");
        }
    }

    Ok(results)
}

#[tracing::instrument(skip_all)]
pub async fn resolve_statics(brioche: &mut BriocheState) -> Result<(), LoadProjectError> {
    for (unresolved, (static_ref, mut locations)) in
        brioche.projects.static_ref_by_unresolved_static.clone()
    {
        let location = locations.swap_remove(0);
        let result = resolve_static(&brioche.resources, &unresolved, location).await;

        match result {
            Ok(static_) => {
                brioche.projects.unresolved_statics.remove(&static_ref);
                brioche
                    .projects
                    .static_ref_by_unresolved_static
                    .remove(&unresolved);

                let module_refs = brioche
                    .projects
                    .graph
                    .edges_directed(static_ref.0, petgraph::Direction::Incoming)
                    .filter_map(|edge| {
                        if let ProjectEdge::ModuleStatic(_) = edge.weight() {
                            Some(ModuleRef(edge.source()))
                        } else {
                            None
                        }
                    });
                let project_refs =
                    module_refs.map(|module_ref| &brioche.projects.project_by_module[&module_ref]);
                for (project_ref, _) in project_refs {
                    let Some(project) = brioche.projects.projects.get_mut(project_ref) else {
                        continue;
                    };

                    project.lockfile_state.update(|lockfile| match &static_ {
                        SharedStatic::Download { url, hash } => {
                            lockfile.downloads.insert(url.clone(), hash.clone());
                        }
                        SharedStatic::GitRef {
                            repository,
                            ref_,
                            commit,
                        } => {
                            lockfile
                                .git_refs
                                .entry(repository.clone())
                                .or_default()
                                .insert(ref_.clone(), commit.clone());
                        }
                    });
                }

                match brioche.projects.static_ref_by_shared_static.entry(static_) {
                    std::collections::hash_map::Entry::Occupied(entry) => {
                        let resolved_ref = *entry.get();
                        brioche
                            .projects
                            .resolved_statics
                            .insert(static_ref, resolved_ref);

                        brioche.projects.graph.add_edge(
                            static_ref.0,
                            resolved_ref.0,
                            ProjectEdge::ResolvedStatic,
                        );
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let static_ = entry.key().clone().into();
                        entry.insert(static_ref);
                        brioche.projects.statics.insert(static_ref, static_);

                        let static_node = brioche
                            .projects
                            .graph
                            .node_weight_mut(static_ref.0)
                            .expect("node not found");
                        *static_node = ProjectNode::Static;
                    }
                }
            }
            Err(issue) => {
                brioche
                    .projects
                    .issues
                    .entry(static_ref.0)
                    .or_default()
                    .push(issue);
            }
        }
    }

    for (static_ref, static_) in &brioche.projects.statics {
        match static_ {
            Static::IncludeFile(relative_path) => {
                let module_ref = brioche.projects.module_for_static(*static_ref);
                let Some((module_ref, range)) = module_ref else {
                    continue;
                };

                let (project_ref, module_subpath) =
                    &brioche.projects.project_by_module[&module_ref];
                let module_dir = module_subpath.parent().expect("invalid module subpath");
                let project_path = &brioche.projects.local_project_paths[project_ref];
                let static_subpath = module_dir.join(relative_path.clone());
                let static_path = project_path.join_subpath(&static_subpath);
                let Ok(static_path) = static_path else {
                    brioche
                        .projects
                        .issues
                        .entry(static_ref.0)
                        .or_default()
                        .push(ProjectIssue::StaticIncludeEscapesProjectPath {
                            static_ref: *static_ref,
                            include: relative_path.clone(),
                            module_ref,
                            range,
                        });
                    continue;
                };
                let static_system_path = static_path.to_system_path()?;

                let file_with_metadata = tokio::fs::File::open(&static_system_path)
                    .and_then(async |file| {
                        let metadata = file.metadata().await?;
                        Ok((file, metadata))
                    })
                    .await;
                let (_file, file_metadata) = match file_with_metadata {
                    Ok(file_with_metadata) => file_with_metadata,
                    Err(error) => {
                        let location = ProjectIssueLocation {
                            source: (*static_ref).into(),
                            range: None,
                        };
                        brioche
                            .projects
                            .issues
                            .entry(static_ref.0)
                            .or_default()
                            .push(ProjectIssue::IoError {
                                error,
                                reason: format!(
                                    "failed to open static file '{}'",
                                    static_system_path.display()
                                )
                                .into(),
                                location,
                            });
                        continue;
                    }
                };

                // TODO: Read file into an artifact

                if !file_metadata.is_file() {
                    brioche
                        .projects
                        .issues
                        .entry(static_ref.0)
                        .or_default()
                        .push(ProjectIssue::StaticIncludeExpectedFile {
                            static_ref: *static_ref,
                            include: relative_path.clone(),
                            module_ref,
                            range,
                        });
                }
            }
            Static::IncludeDirectory(relative_path) => {
                let module_ref = brioche.projects.module_for_static(*static_ref);
                let Some((module_ref, range)) = module_ref else {
                    continue;
                };

                let (project_ref, module_subpath) =
                    &brioche.projects.project_by_module[&module_ref];
                let module_dir = module_subpath.parent().expect("invalid module subpath");
                let project_path = &brioche.projects.local_project_paths[project_ref];
                let static_subpath = module_dir.join(relative_path.clone());
                let static_path = project_path.join_subpath(&static_subpath);
                let Ok(static_path) = static_path else {
                    brioche
                        .projects
                        .issues
                        .entry(static_ref.0)
                        .or_default()
                        .push(ProjectIssue::StaticIncludeEscapesProjectPath {
                            static_ref: *static_ref,
                            include: relative_path.clone(),
                            module_ref,
                            range,
                        });
                    continue;
                };
                let static_system_path = static_path.to_system_path()?;

                let directory_metadata = tokio::fs::metadata(&static_system_path).await;
                let directory_metadata = match directory_metadata {
                    Ok(directory_metadata) => directory_metadata,
                    Err(error) => {
                        let location = ProjectIssueLocation {
                            source: (*static_ref).into(),
                            range: None,
                        };
                        brioche
                            .projects
                            .issues
                            .entry(static_ref.0)
                            .or_default()
                            .push(ProjectIssue::IoError {
                                error,
                                reason: format!(
                                    "failed to read static directory '{}'",
                                    static_system_path.display()
                                )
                                .into(),
                                location,
                            });
                        continue;
                    }
                };

                // TODO: Read directory into an artifact

                if !directory_metadata.is_dir() {
                    brioche
                        .projects
                        .issues
                        .entry(static_ref.0)
                        .or_default()
                        .push(ProjectIssue::StaticIncludeExpectedDirectory {
                            static_ref: *static_ref,
                            include: relative_path.clone(),
                            module_ref,
                            range,
                        });
                }
            }
            Static::Glob { patterns: _ }
            | Static::Download { url: _, hash: _ }
            | Static::GitRef {
                repository: _,
                ref_: _,
                commit: _,
            } => {
                // TODO: Construct artifact
            }
        }
    }

    Ok(())
}

async fn resolve_static(
    brioche: &BriocheResources,
    static_: &UnresolvedStatic,
    location: ProjectIssueLocation,
) -> Result<SharedStatic, ProjectIssue> {
    match static_ {
        UnresolvedStatic::Download { url } => {
            let new_blob_hash =
                crate::download::download(brioche, url, None, JobContext::default())
                    .await
                    .map_err(|error| ProjectIssue::DownloadError {
                        error,
                        url: url.clone(),
                        location,
                    })?;
            let blob_system_path = crate::blob::local_blob_path(brioche, new_blob_hash);
            let mut blob = tokio::fs::File::open(&blob_system_path)
                .await
                .map_err(|error| ProjectIssue::IoError {
                    error,
                    reason: format!(
                        "failed to open blob '{}' for download '{url}'",
                        blob_system_path.display()
                    )
                    .into(),
                    location,
                })?;

            let mut hasher = crate::hash::AnyHashHasher::new_sha256();
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                let length =
                    blob.read(&mut buffer)
                        .await
                        .map_err(|error| ProjectIssue::IoError {
                            error,
                            reason: format!(
                                "error while reading blob '{}'",
                                blob_system_path.display()
                            )
                            .into(),
                            location,
                        })?;
                if length == 0 {
                    break;
                }
                hasher.update(&buffer[..length]);
            }

            let hash = hasher.finish();
            Ok(SharedStatic::Download {
                url: url.clone(),
                hash,
            })
        }
        UnresolvedStatic::GitRef {
            repository: _,
            ref_: _,
        } => todo!(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadProjectError {
    #[error("{reason}: {error}")]
    IoError {
        #[source]
        error: std::io::Error,
        reason: Cow<'static, str>,
    },

    #[error(transparent)]
    ToSystemPathError(#[from] crate::path::ToSystemPathError),

    #[error(transparent)]
    SubpathError(#[from] crate::path::SubpathError),

    #[error(transparent)]
    FromSystemPathError(#[from] crate::path::FromSystemPathError),

    #[error(transparent)]
    RegistryError(#[from] crate::registry::RegistryError),
}

impl From<FindWorkspaceRootError> for LoadProjectError {
    fn from(error: FindWorkspaceRootError) -> Self {
        match error {
            FindWorkspaceRootError::Io { error, reason } => Self::IoError { error, reason },
            FindWorkspaceRootError::Subpath(error) => Self::SubpathError(error),
            FindWorkspaceRootError::ToSystemPath(error) => Self::ToSystemPathError(error),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LoadModuleError {
    #[error("IO error at {}: {error_message}", .path.display())]
    IoError {
        error_message: String,
        path: std::path::PathBuf,
    },
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
pub enum LockfileIssue {
    #[error("lockfile not found")]
    NotFound,
    #[error(transparent)]
    Utf8Error(std::str::Utf8Error),
    #[error(transparent)]
    DeserializeError(serde_json::Error),
}

async fn find_workspace_root(
    top: &AbsolutePath,
    path: &RelativePath,
) -> Result<Option<RelativePath>, FindWorkspaceRootError> {
    let mut current_path = path.clone();
    loop {
        let workspace_definition_path = top
            .join_subpath(&current_path)?
            .join_one("brioche_workspace.toml");
        let workspace_definition_system_path = workspace_definition_path.to_system_path()?;
        let exists = tokio::fs::try_exists(&workspace_definition_system_path)
            .await
            .map_err(|error| FindWorkspaceRootError::Io {
                error,
                reason: format!("failed to check path metadata for '{workspace_definition_path}'")
                    .into(),
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

async fn find_workspace_root_absolute(
    path: &AbsolutePath,
) -> Result<Option<AbsolutePath>, FindWorkspaceRootError> {
    let root_path = path.root_path().clone().into();
    let workspace_root = find_workspace_root(&root_path, &path.subpath()).await?;
    let Some(workspace_root) = workspace_root else {
        return Ok(None);
    };

    let workspace_root = root_path.join_subpath(&workspace_root)?;
    Ok(Some(workspace_root))
}

#[derive(Debug, thiserror::Error)]
enum FindWorkspaceRootError {
    #[error("{reason}: {error}")]
    Io {
        error: std::io::Error,
        reason: Cow<'static, str>,
    },

    #[error(transparent)]
    Subpath(#[from] crate::path::SubpathError),

    #[error(transparent)]
    ToSystemPath(#[from] crate::path::ToSystemPathError),
}

async fn load_module_source(path: &std::path::Path) -> Result<Arc<str>, LoadModuleError> {
    let source = tokio::fs::read(path)
        .await
        .map_err(|error| LoadModuleError::IoError {
            error_message: error.to_string(),
            path: path.to_path_buf(),
        })?;
    let source = String::from_utf8(source)
        .map_err(|error| LoadModuleError::Utf8Error(error.utf8_error()))?;
    Ok(source.into())
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

async fn load_project_by_hash(
    brioche: &mut BriocheState,
    project_hash: ProjectHash,
) -> Result<(AbsolutePath, Option<AbsolutePath>), LoadProjectByHashError> {
    // Use a mutex to ensure we don't try to fetch the same project more
    // than once at a time
    static FETCH_PROJECTS_MUTEX: tokio::sync::Mutex<
        BTreeMap<ProjectHash, Arc<tokio::sync::Mutex<()>>>,
    > = tokio::sync::Mutex::const_new(BTreeMap::new());
    let project_mutex = {
        let mut fetch_projects = FETCH_PROJECTS_MUTEX.lock().await;
        fetch_projects.entry(project_hash).or_default().clone()
    };
    let _guard = project_mutex.lock().await;

    let projects_system_path = brioche.resources.data_dir.join("projects");
    tokio::fs::create_dir_all(&projects_system_path)
        .await
        .map_err(|error| LoadProjectByHashError::IoError {
            error,
            reason: format!(
                "failed to create directory '{}'",
                projects_system_path.display()
            )
            .into(),
        })?;
    let projects_path = crate::path::canonicalize_system_path(&projects_system_path).await?;
    let local_system_path = projects_system_path.join(project_hash.to_string());
    let local_path = crate::path::canonicalize_system_path(&local_system_path).await;

    match local_path {
        Ok(local_path) => {
            // Directory for the local project exists. No need to fetch. The
            // hash is also validated later on

            let project_path = crate::path::relative_path_between(&projects_path, &local_path)?;
            let workspace_root = find_workspace_root(&projects_path, &project_path).await?;
            let workspace_root = workspace_root
                .map(|subpath| projects_path.join_subpath(&subpath))
                .transpose()?;

            return Ok((local_path, workspace_root));
        }
        Err(crate::path::FromSystemPathError::IoError { error, .. })
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            // Directory for the local project does not exist
        }
        Err(error) => {
            return Err(LoadProjectByHashError::FromSystemPathError(error));
        }
    }

    // By this point, we know the project doesn't exist locally so we
    // need to fetch it.

    let artifact_hash = crate::cache::load_project_artifact_hash(brioche, project_hash)
        .await?
        .ok_or(LoadProjectByHashError::ProjectHashNotFoundInCache)?;
    let artifact_ref = crate::cache::load_artifact(
        brioche,
        artifact_hash,
        crate::reporter::job::CacheFetchKind::Project,
        JobContext::default(),
    )
    .await?
    .ok_or_else(|| LoadProjectByHashError::ProjectArtifactNotFoundInCache { artifact_hash })?;

    let mut saved_projects =
        super::artifact::save_projects_from_artifact(brioche, artifact_ref).await?;
    let Some(project_path) = saved_projects.remove(&project_hash) else {
        return Err(LoadProjectByHashError::ProjectNotFoundInArtifact { artifact_hash });
    };

    let project_relative_path = crate::path::relative_path_between(&projects_path, &project_path).unwrap_or_else(|error| panic!("failed to normalize project path '{project_path}' relative to projects dir '{projects_path}': {error}"));
    let workspace_root = find_workspace_root(&projects_path, &project_relative_path).await?;
    let workspace_root = workspace_root
        .map(|subpath| projects_path.join_subpath(&subpath))
        .transpose()?;

    Ok((project_path, workspace_root))
}

#[derive(Debug, thiserror::Error)]
pub enum LoadProjectByHashError {
    #[error("{reason}: {error}")]
    IoError {
        #[source]
        error: std::io::Error,
        reason: Cow<'static, str>,
    },

    #[error("project hash not found in cache")]
    ProjectHashNotFoundInCache,

    #[error("resolved project to artifact {artifact_hash}, but artifact hash not found in cache")]
    ProjectArtifactNotFoundInCache { artifact_hash: RecipeHash },

    #[error("project not found in artifact {artifact_hash} retrieved from cache")]
    ProjectNotFoundInArtifact { artifact_hash: RecipeHash },

    #[error(transparent)]
    CacheError(#[from] crate::cache::CacheError),

    #[error(transparent)]
    SaveProjectsFromArtifactError(#[from] crate::project::artifact::SaveProjectsFromArtifactError),

    #[error(transparent)]
    SubpathError(#[from] crate::path::SubpathError),

    #[error(transparent)]
    ToSystemPathError(#[from] crate::path::ToSystemPathError),

    #[error(transparent)]
    FromSystemPathError(#[from] crate::path::FromSystemPathError),

    #[error(transparent)]
    RelativePathBetweenError(#[from] crate::path::RelativePathBetweenError),
}

impl From<FindWorkspaceRootError> for LoadProjectByHashError {
    fn from(error: FindWorkspaceRootError) -> Self {
        match error {
            FindWorkspaceRootError::Io { error, reason } => Self::IoError { error, reason },
            FindWorkspaceRootError::Subpath(error) => Self::SubpathError(error),
            FindWorkspaceRootError::ToSystemPath(error) => Self::ToSystemPathError(error),
        }
    }
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

struct ResolveProjectDependencyContext<'a> {
    project_path: &'a AbsolutePath,
    project_definition: &'a ProjectDefinition,
    workspace: Option<&'a Workspace>,
    external_deps: &'a mut HashMap<String, Option<ProjectSpecifier>>,
    issues: &'a mut Vec<ProjectIssue>,
    lockfile: Option<&'a Lockfile>,
    new_lockfile: &'a mut Lockfile,
}

async fn resolve_project_dependency(
    brioche: &BriocheResources,
    ctx: &mut ResolveProjectDependencyContext<'_>,
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

    if let Some(resolved) = resolve_project_from_workspace(ctx, specifier, &location).await {
        ctx.external_deps
            .insert(specifier.to_string(), Some(resolved.clone()));
        return Some(resolved);
    }

    if let Some(lockfile) = ctx.lockfile
        && let Some(project_hash) = lockfile.dependencies.get(specifier)
    {
        ctx.new_lockfile
            .dependencies
            .insert(specifier.to_string(), *project_hash);

        let resolved = ProjectSpecifier::Hash(*project_hash);
        ctx.external_deps
            .insert(specifier.to_string(), Some(resolved.clone()));
        return Some(resolved);
    }

    let registry_response = crate::registry::get_project_tag(brioche, specifier, "latest").await;

    match registry_response {
        Ok(Some(registry_response)) => {
            ctx.new_lockfile
                .dependencies
                .insert(specifier.to_string(), registry_response.project_hash);

            let resolved = ProjectSpecifier::Hash(registry_response.project_hash);
            ctx.external_deps
                .insert(specifier.to_string(), Some(resolved.clone()));
            return Some(resolved);
        }
        Ok(None) => {
            ctx.issues.push(ProjectIssue::DependencyNotFound {
                dependency: specifier.to_string(),
                location,
            });
        }
        Err(error) => {
            ctx.issues
                .push(ProjectIssue::RegistryError { error, location });
        }
    }

    None
}

async fn resolve_project_from_workspace(
    ctx: &mut ResolveProjectDependencyContext<'_>,
    specifier: &str,
    location: &ProjectIssueLocation,
) -> Option<ProjectSpecifier> {
    let workspace = ctx.workspace?;
    for member in &workspace.definition.members {
        match member {
            WorkspaceMember::Path(parent, name) => {
                if name == specifier {
                    let member_path = workspace
                        .root
                        .join_subpath(parent)
                        .expect("invalid workspace member subpath")
                        .join_one(name);
                    return Some(ProjectSpecifier::Path(member_path));
                }
            }
            WorkspaceMember::WildcardPath(parent) => {
                let member_path = workspace
                    .root
                    .join_subpath(parent)
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
                            location: *location,
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
                            error,
                            reason: format!(
                                "failed to check for wildcard workspace member at '{}'",
                                root_module_system_path.display()
                            )
                            .into(),
                            location: *location,
                        });
                    }
                }
            }
        }
    }

    None
}

fn prepare_static(static_query: StaticQuery, lockfile: Option<&Lockfile>) -> PartialStatic {
    match static_query {
        StaticQuery::IncludeFile(path) => PartialStatic::Unique(Static::IncludeFile(path)),
        StaticQuery::IncludeDirectory(path) => {
            PartialStatic::Unique(Static::IncludeDirectory(path))
        }
        StaticQuery::Glob { patterns } => PartialStatic::Unique(Static::Glob { patterns }),
        StaticQuery::Download { url } => {
            if let Some(lockfile) = lockfile
                && let Some(hash) = lockfile.downloads.get(&url)
            {
                PartialStatic::Shared(SharedStatic::Download {
                    url,
                    hash: hash.clone(),
                })
            } else {
                PartialStatic::Unresolved(UnresolvedStatic::Download { url })
            }
        }
        StaticQuery::GitRef(options) => {
            if let Some(lockfile) = lockfile
                && let Some(commits) = lockfile.git_refs.get(&options.repository)
                && let Some(commit) = commits.get(&options.ref_)
            {
                PartialStatic::Shared(SharedStatic::GitRef {
                    repository: options.repository,
                    ref_: options.ref_,
                    commit: commit.clone(),
                })
            } else {
                PartialStatic::Unresolved(UnresolvedStatic::GitRef {
                    repository: options.repository,
                    ref_: options.ref_,
                })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum PartialStatic {
    Unresolved(UnresolvedStatic),
    Shared(SharedStatic),
    Unique(Static),
}
