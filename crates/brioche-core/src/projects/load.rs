use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
};

use futures::TryFutureExt as _;
use petgraph::visit::EdgeRef as _;
use tokio::io::AsyncReadExt as _;

use crate::{
    Brioche,
    path::{AbsolutePath, RelativePath},
    projects::{
        DependencyDefinition, Lockfile, Module, ModuleRef, ModuleReferrer, Project,
        ProjectDefinition, ProjectEdge, ProjectIssue, ProjectIssueLocation, ProjectNode,
        ProjectRef, ProjectReferrer, ProjectSpecifier, SharedStatic, Static, StaticQuery,
        StaticRef, UnresolvedStatic, Version, Workspace, WorkspaceDefinition, WorkspaceMember,
        WorkspaceRef, hash::ProjectHash,
    },
    reporter::job::JobContext,
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
    let mut project_hashes_to_validate = HashMap::new();
    let mut results = HashMap::new();

    let mut shared_statics = HashMap::<SharedStatic, StaticRef>::new();

    while let Some((specifier, referrer)) = queue.pop_front() {
        if let Some(project) = projects.projects_by_specifier.get(&specifier) {
            tracing::trace!(?specifier, ?referrer, "project already loaded");

            match referrer {
                ProjectReferrer::TopLevel => {
                    results.insert(specifier, *project);
                }
                ProjectReferrer::Project { referrer, edge, .. } => {
                    projects.graph.update_edge(referrer.0, project.0, edge);
                }
            }

            continue;
        }

        let project_ref = projects.graph.add_node(ProjectNode::Project);
        let project_ref = ProjectRef(project_ref);

        projects
            .projects_by_specifier
            .insert(specifier.clone(), project_ref);

        tracing::debug!(?specifier, ?referrer, "loading project");

        let (project_path, workspace_root) = match &specifier {
            ProjectSpecifier::Path(path) => {
                let workspace_root = find_workspace_root(path).await?;

                tracing::trace!(?path, ?workspace_root, "searched for workspace root");

                (path.clone(), workspace_root)
            }
            ProjectSpecifier::Hash(project_hash) => {
                project_hashes_to_validate.insert(project_ref, *project_hash);

                match load_project_by_hash(brioche, *project_hash).await {
                    Ok((path, workspace_root)) => (path, workspace_root),
                    Err(error) => {
                        todo!("add project issue: {error:#?}");
                        // projects.issues.entry(project_ref.0).or_default().push(ProjectIssue::IoError { error_message: (), path: (), location: () })
                    }
                }
            }
        };

        projects
            .local_project_paths
            .insert(project_ref, project_path.clone());

        match &referrer {
            ProjectReferrer::TopLevel => {
                results.insert(specifier.clone(), project_ref);
            }
            ProjectReferrer::Project { referrer, edge, .. } => {
                projects
                    .graph
                    .update_edge(referrer.0, project_ref.0, edge.clone());
            }
        }

        let workspace_entry;
        let workspace = if let Some(workspace_root) = workspace_root {
            match projects.workspaces_by_path.entry(workspace_root) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    projects.graph.update_edge(
                        entry.get().0,
                        project_ref.0,
                        ProjectEdge::ProjectWithinWorkspace,
                    );

                    let workspace_ref = *entry.get();
                    projects.workspaces[&workspace_ref].as_ref().ok()
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let workspace_ref = projects.graph.add_node(ProjectNode::Workspace);
                    let workspace_ref = WorkspaceRef(workspace_ref);

                    projects.graph.update_edge(
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

        let lockfile_subpath = RelativePath::one("brioche.lock");
        let lockfile_path = project_path.join_subpath(lockfile_subpath.clone()).unwrap();
        let lockfile_system_path = lockfile_path.to_system_path()?;
        let lockfile_content = tokio::fs::read(&lockfile_system_path).await;
        let lockfile_content = match lockfile_content {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(LoadProjectError::IoError {
                    error,
                    path: lockfile_path,
                });
            }
        };
        let lockfile = lockfile_content.as_deref().map_or_else(
            || Err(LockfileIssue::NotFound),
            |content| {
                let content = std::str::from_utf8(content).map_err(LockfileIssue::Utf8Error)?;
                let lockfile: Lockfile =
                    serde_json::from_str(content).map_err(LockfileIssue::DeserializeError)?;
                Ok(lockfile)
            },
        );
        let mut new_lockfile = Lockfile::default();

        let root_module_subpath = RelativePath::one("project.bri");
        let root_module_path = project_path
            .join_subpath(root_module_subpath.clone())
            .unwrap();

        let mut project_definition = ProjectDefinition::default();
        let mut project_modules = HashMap::<RelativePath, ModuleRef>::new();
        let mut external_deps = HashMap::<String, Option<ProjectSpecifier>>::new();
        let mut module_asts = HashMap::<ModuleRef, crate::script::parse::ScriptAst>::new();

        let mut module_queue = VecDeque::from_iter([(
            root_module_subpath.clone(),
            ModuleReferrer::ProjectRoot { project_ref },
        )]);
        while let Some((module_subpath, module_referrer)) = module_queue.pop_front() {
            if let Some(module_ref) = project_modules.get(&module_subpath) {
                projects.graph.update_edge(
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
            projects.graph.update_edge(
                module_referrer.node_index(),
                module_ref.0,
                module_referrer.edge(),
            );
            project_modules.insert(module_subpath.clone(), module_ref);
            projects
                .project_by_module
                .insert(module_ref, (project_ref, module_subpath.clone()));

            let module_system_path = module_path.to_system_path()?;
            let module_source = load_module_source(&module_system_path).await;
            let module = Module {
                project: project_ref,
                source: module_source,
                subpath: module_subpath,
            };

            let module_entry = projects.modules.entry(module_ref).insert_entry(module);
            let module = module_entry.get();

            let module_ast = module
                .source
                .as_deref()
                .map(crate::script::parse::parse_script);
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
                                projects.issues.entry(module_ref.0).or_default().push(
                                    ProjectIssue::ScriptParseError {
                                        error,
                                        path: root_module_path.clone(),
                                    },
                                );
                                None
                            }
                        };

                        // Insert the `project` export in the env, so it can
                        // be referenced when resolving statics
                        if let Some(project) = &project_definition_value {
                            env.insert("project".to_string(), project.value.clone());
                        }

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
                                let resolved_dep = resolve_project_dependency(
                                    brioche,
                                    &mut ResolveProjectDependencyContext {
                                        project_path: &project_path,
                                        project_definition: &project_definition,
                                        workspace,
                                        external_deps: &mut external_deps,
                                        issues,
                                        lockfile: lockfile.as_ref().ok(),
                                        new_lockfile: &mut new_lockfile,
                                    },
                                    specifier,
                                    location.clone(),
                                )
                                .await;

                                if let Some(resolved_dep) = resolved_dep {
                                    queue.push_back((
                                        resolved_dep,
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

                    let static_queries = crate::script::parse::find_statics(module_ast, &env);
                    for query in static_queries {
                        let query = match query {
                            Ok(query) => query,
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
                        let static_ = prepare_static(query.query.clone(), lockfile.as_ref().ok());

                        let static_ref = match static_ {
                            PartialStatic::Shared(static_) => {
                                *shared_statics.entry(static_).or_insert_with_key(|static_| {
                                    let static_ref =
                                        StaticRef(projects.graph.add_node(ProjectNode::Static));
                                    projects.statics.insert(static_ref, static_.clone().into());
                                    static_ref
                                })
                            }
                            PartialStatic::Unique(static_) => {
                                let static_ref =
                                    StaticRef(projects.graph.add_node(ProjectNode::Static));
                                projects.statics.insert(static_ref, static_);
                                static_ref
                            }
                            PartialStatic::Unresolved(static_) => {
                                let location = ProjectIssueLocation {
                                    path: module_path.clone(),
                                    range: Some(query.range),
                                };
                                let static_ref_entry = projects
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
                                            projects.graph.add_node(ProjectNode::UnresolvedStatic),
                                        );
                                        entry.insert((static_ref, vec![location]));
                                        static_ref
                                    }
                                };
                                projects.unresolved_statics.insert(static_ref, static_);
                                static_ref
                            }
                        };

                        projects.graph.add_edge(
                            module_ref.0,
                            static_ref.0,
                            ProjectEdge::ModuleStatic(query),
                        );
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
                        ProjectIssue::LoadModuleError {
                            error: (*error).clone(),
                            path: module_path,
                            location,
                        },
                    );
                }
            }
        }

        let root_module_ref = &project_modules[&root_module_subpath];
        let root_module_ast = module_asts.get(root_module_ref);

        let project_definition_value = root_module_ast.map_or_else(
            || Ok(None),
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
            let resolved_dep = resolve_project_dependency(
                brioche,
                &mut ResolveProjectDependencyContext {
                    project_path: &project_path,
                    project_definition: &project_definition,
                    workspace,
                    external_deps: &mut external_deps,
                    issues,
                    lockfile: lockfile.as_ref().ok(),
                    new_lockfile: &mut new_lockfile,
                },
                specifier,
                project_definition_location.clone(),
            )
            .await;

            if let Some(resolved_dep) = resolved_dep {
                queue.push_back((
                    resolved_dep,
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
            lockfile: new_lockfile,
        };
        projects.projects.insert(project_ref, project);

        projects
            .modules_by_project
            .insert(project_ref, project_modules);
    }

    // Skip project hash validation for any projects that already had
    // other issues
    project_hashes_to_validate.retain(|project_ref, _| {
        projects
            .issues
            .get(&project_ref.0)
            .is_none_or(Vec::is_empty)
    });

    if !project_hashes_to_validate.is_empty() {
        // Create a copy of the graph, but keeping only project nodes to
        // validate
        let mut graph = projects.graph.clone();
        let mut dfs_space = petgraph::algo::DfsSpace::default();
        graph.retain_nodes(|graph, index| match &graph[index] {
            crate::projects::ProjectNode::Project => {
                project_hashes_to_validate.keys().any(|project_ref| {
                    petgraph::algo::has_path_connecting(
                        &*graph,
                        project_ref.0,
                        index,
                        Some(&mut dfs_space),
                    )
                })
            }
            crate::projects::ProjectNode::Workspace
            | crate::projects::ProjectNode::Module
            | crate::projects::ProjectNode::Static
            | crate::projects::ProjectNode::UnresolvedStatic => false,
        });

        // Group nodes by finding the strongly-connected components of the graph.
        // This effectively finds cyclic projects in the graph that we should
        // group together, and puts acyclic projects into a group of one element.
        // The result is additionally topographically sorted, so every project
        // naturally comes after all of its dependencies
        let node_groups = petgraph::algo::tarjan_scc(&graph);

        let mut recipes = brioche.recipes.write().await;
        let mut permit = crate::blob::get_save_blob_permit()
            .await
            .expect("todo: failed to get save blob permit");

        let mut project_hashes = HashMap::new();
        crate::projects::hash::hash_projects_inner(
            brioche,
            projects,
            &mut recipes,
            &mut permit,
            &node_groups,
            &mut project_hashes,
        );

        for (project_ref, expected_hash) in project_hashes_to_validate {
            let actual_hash = project_hashes[&project_ref];
            if expected_hash != actual_hash {
                projects.issues.entry(project_ref.0).or_default().push(
                    ProjectIssue::ProjectHashMismatch {
                        expected_hash,
                        actual_hash,
                    },
                );
            }
        }
    }

    Ok(results)
}

#[tracing::instrument(skip_all)]
pub async fn resolve_statics(brioche: &Brioche) -> Result<(), LoadProjectError> {
    let mut projects = brioche.projects.write().await;

    for (unresolved, (static_ref, mut locations)) in
        projects.static_ref_by_unresolved_static.clone()
    {
        let location = locations.swap_remove(0);
        let result = resolve_static(brioche, &unresolved, location).await;

        match result {
            Ok(static_) => {
                projects.unresolved_statics.remove(&static_ref);
                projects.static_ref_by_unresolved_static.remove(&unresolved);

                match projects.static_ref_by_shared_static.entry(static_) {
                    std::collections::hash_map::Entry::Occupied(entry) => {
                        let resolved_ref = *entry.get();
                        projects.resolved_statics.insert(static_ref, resolved_ref);

                        projects.graph.add_edge(
                            static_ref.0,
                            resolved_ref.0,
                            ProjectEdge::ResolvedStatic,
                        );
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(static_ref);

                        let static_node = projects
                            .graph
                            .node_weight_mut(static_ref.0)
                            .expect("node not found");
                        *static_node = ProjectNode::Static;
                    }
                }
            }
            Err(issue) => {
                projects.issues.entry(static_ref.0).or_default().push(issue);
            }
        }
    }

    let projects = &mut *projects;
    for (static_ref, static_) in &projects.statics {
        match static_ {
            Static::IncludeFile(relative_path) => {
                let module_ref = projects.module_for_static(*static_ref);
                let Some(module_ref) = module_ref else {
                    continue;
                };

                let (project_ref, module_subpath) = &projects.project_by_module[&module_ref];
                let module_dir = module_subpath.parent().expect("invalid module subpath");
                let project_path = &projects.local_project_paths[project_ref];
                let static_subpath = module_dir.join(relative_path.clone());
                let static_path = project_path.join_subpath(static_subpath);
                let Ok(static_path) = static_path else {
                    projects.issues.entry(static_ref.0).or_default().push(
                        ProjectIssue::StaticIncludeEscapesProjectPath {
                            include: relative_path.clone(),
                            module_subpath: module_subpath.clone(),
                        },
                    );
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
                        // TODO: Better location tracking
                        let location = ProjectIssueLocation {
                            path: project_path.join(module_subpath.clone()),
                            range: None,
                        };
                        projects.issues.entry(static_ref.0).or_default().push(
                            ProjectIssue::IoError {
                                error_message: error.to_string(),
                                path: static_path,
                                location,
                            },
                        );
                        continue;
                    }
                };

                // TODO: Read file into an artifact

                if !file_metadata.is_file() {
                    projects.issues.entry(static_ref.0).or_default().push(
                        ProjectIssue::StaticIncludeExpectedFile {
                            include: relative_path.clone(),
                            module_subpath: module_subpath.clone(),
                        },
                    );
                }
            }
            Static::IncludeDirectory(relative_path) => {
                let module_ref = projects.module_for_static(*static_ref);
                let Some(module_ref) = module_ref else {
                    continue;
                };

                let (project_ref, module_subpath) = &projects.project_by_module[&module_ref];
                let module_dir = module_subpath.parent().expect("invalid module subpath");
                let project_path = &projects.local_project_paths[project_ref];
                let static_subpath = module_dir.join(relative_path.clone());
                let static_path = project_path.join_subpath(static_subpath);
                let Ok(static_path) = static_path else {
                    projects.issues.entry(static_ref.0).or_default().push(
                        ProjectIssue::StaticIncludeEscapesProjectPath {
                            include: relative_path.clone(),
                            module_subpath: module_subpath.clone(),
                        },
                    );
                    continue;
                };
                let static_system_path = static_path.to_system_path()?;

                let directory_metadata = tokio::fs::metadata(&static_system_path).await;
                let directory_metadata = match directory_metadata {
                    Ok(directory_metadata) => directory_metadata,
                    Err(error) => {
                        // TODO: Better location tracking
                        let location = ProjectIssueLocation {
                            path: project_path.join(module_subpath.clone()),
                            range: None,
                        };
                        projects.issues.entry(static_ref.0).or_default().push(
                            ProjectIssue::IoError {
                                error_message: error.to_string(),
                                path: static_path,
                                location,
                            },
                        );
                        continue;
                    }
                };

                // TODO: Read directory into an artifact

                if !directory_metadata.is_dir() {
                    projects.issues.entry(static_ref.0).or_default().push(
                        ProjectIssue::StaticIncludeExpectedDirectory {
                            include: relative_path.clone(),
                            module_subpath: module_subpath.clone(),
                        },
                    );
                }
            }
            Static::Glob { patterns } => {
                // TODO: Construct artifact
            }
            Static::Download { url, hash } => {
                // TODO: Construct artifact
            }
            Static::GitRef {
                repository,
                ref_,
                commit,
            } => {
                // TODO: Construct artifact
            }
        }
    }

    Ok(())
}

async fn resolve_static(
    brioche: &Brioche,
    static_: &UnresolvedStatic,
    location: ProjectIssueLocation,
) -> Result<SharedStatic, ProjectIssue> {
    match static_ {
        UnresolvedStatic::Download { url } => {
            let new_blob_hash =
                crate::download::download(brioche, &url, None, JobContext::default())
                    .await
                    .map_err(|error| ProjectIssue::DownloadError {
                        url: url.clone(),
                        error_message: error.to_string(),
                        location: location.clone(),
                    })?;
            let blob_system_path = crate::blob::local_blob_path(brioche, new_blob_hash);
            let blob_path = crate::path::canonicalize_system_path(&blob_system_path)
                .await
                .expect("failed to canonicalize blob path");
            let mut blob = tokio::fs::File::open(&blob_system_path)
                .await
                .map_err(|error| ProjectIssue::IoError {
                    error_message: error.to_string(),
                    path: blob_path,
                    location: location.clone(),
                })?;

            let mut hasher = crate::hash::AnyHashHasher::new_sha256();
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                let length =
                    blob.read(&mut buffer)
                        .await
                        .map_err(|error| ProjectIssue::DownloadError {
                            url: url.clone(),
                            error_message: error.to_string(),
                            location: location.clone(),
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
        UnresolvedStatic::GitRef { repository, ref_ } => todo!(),
    }
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

    #[error(transparent)]
    RegistryError(#[from] crate::registry::RegistryError),
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

async fn load_module_source(path: &std::path::Path) -> Result<String, LoadModuleError> {
    let source = tokio::fs::read(path)
        .await
        .map_err(|error| LoadModuleError::IoError {
            error_message: error.to_string(),
        })?;
    let source = String::from_utf8(source)
        .map_err(|error| LoadModuleError::Utf8Error(error.utf8_error()))?;
    Ok(source)
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
    brioche: &Brioche,
    project_hash: ProjectHash,
) -> Result<(AbsolutePath, Option<AbsolutePath>), ProjectIssue> {
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

    // TODO: handle errors cleanly
    let projects_system_path = brioche.data_dir.join("projects");
    tokio::fs::create_dir_all(&projects_system_path)
        .await
        .unwrap();
    let projects_path = crate::path::canonicalize_system_path(&projects_system_path)
        .await
        .unwrap();
    let local_path = projects_path.join_one(project_hash.to_string());
    let local_system_path = local_path.to_system_path().unwrap();

    let local_project_exists =
        tokio::fs::try_exists(&local_system_path)
            .await
            .map_err(|error| ProjectIssue::IoError {
                error_message: error.to_string(),
                path: local_path.clone(),
                location: ProjectIssueLocation {
                    path: local_path.clone(),
                    range: None,
                },
            })?;
    if local_project_exists {
        // Directory for the local project exists. No need to fetch. The
        // hash is also validated later on
        // TODO: workspace
        return Ok((local_path, None));
    }

    // By this point, we know the project doesn't exist locally so we
    // need to fetch it.

    let artifact_hash = crate::cache::load_project_artifact_hash(brioche, project_hash)
        .await
        .map_err(|error| ProjectIssue::CacheError {
            error_message: error.to_string(),
        })?
        .ok_or_else(|| ProjectIssue::CacheError {
            error_message: "project not found in cache".to_string(),
        })?;
    let artifact_ref = crate::cache::load_artifact(
        brioche,
        artifact_hash,
        crate::reporter::job::CacheFetchKind::Project,
    )
    .await
    .map_err(|error| ProjectIssue::CacheError {
        error_message: error.to_string(),
    })?
    .ok_or_else(|| ProjectIssue::CacheError {
        error_message: "no artifact found for project in cache".to_string(),
    })?;

    let mut saved_projects = super::artifact::save_projects_from_artifact(brioche, artifact_ref)
        .await
        .map_err(|error| ProjectIssue::CacheError {
            error_message: error.to_string(),
        })?;
    let Some(project_path) = saved_projects.remove(&project_hash) else {
        return Err(ProjectIssue::CacheError {
            error_message: format!(
                "artifact for project found in cache, but it did not contain the project {project_hash}"
            ),
        });
    };

    // TODO: Workspace!
    Ok((project_path, None))
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
    brioche: &Brioche,
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
        Ok(None) => {}
        Err(error) => {
            ctx.issues.push(ProjectIssue::RegistryError {
                error,
                location: location.clone(),
            });
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

enum PartialStatic {
    Unresolved(UnresolvedStatic),
    Shared(SharedStatic),
    Unique(Static),
}
