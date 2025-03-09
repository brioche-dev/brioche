use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use petgraph::visit::EdgeRef as _;
use relative_path::{PathExt as _, RelativePathBuf};
use tokio::io::AsyncReadExt as _;

use crate::{
    Brioche,
    project::{DependencyRef, ProjectEntry, Workspace, WorkspaceHash},
    recipe::Artifact,
};

use super::{
    DependencyDefinition, Lockfile, Project, ProjectHash, ProjectLocking, ProjectValidation,
    Projects, Version, WorkspaceDefinition, WorkspaceMember,
    analyze::{GitRefOptions, ProjectAnalysis, StaticOutput, StaticOutputKind, StaticQuery},
};

pub async fn load_project(
    projects: Projects,
    brioche: Brioche,
    path: PathBuf,
    validation: ProjectValidation,
    locking: ProjectLocking,
    depth: u32,
) -> anyhow::Result<ProjectHash> {
    let rt = tokio::runtime::Handle::current();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let local_set = tokio::task::LocalSet::new();

        local_set.spawn_local(async move {
            let result = Box::pin(load_project_inner(
                &LoadProjectConfig {
                    projects,
                    brioche,
                    locking,
                    validation,
                },
                &path,
                depth,
            ))
            .await;
            let _ = tx.send(result).inspect_err(|err| {
                tracing::warn!("failed to send project load result: {err:?}");
            });
        });

        rt.block_on(local_set);
    });

    let (project_hash, _, _) = rx.await.context("failed to get project load result")??;
    Ok(project_hash)
}

struct ProjectNodeDetails {
    project_analysis: ProjectAnalysis,
    lockfile: LockfileState,
    lockfile_path: PathBuf,
    workspace: Option<WorkspaceInfo>,
    dependency_errors: HashMap<String, anyhow::Error>,
}

#[derive(Debug)]
enum ProjectNode {
    Project(PathBuf),
    InitializeWorkspace(PathBuf),
    FinishWorkspace(PathBuf),
}

impl ProjectNode {
    fn path(&self) -> &Path {
        match self {
            ProjectNode::Project(path) => path,
            ProjectNode::InitializeWorkspace(path) => path,
            ProjectNode::FinishWorkspace(path) => path,
        }
    }
}

#[derive(Debug)]
enum ProjectEdge {
    DirectDependency { name: String },
    WorkspaceDependency { name: String, path: PathBuf },
    Other,
}

struct ProjectGraph {
    graph: petgraph::stable_graph::StableDiGraph<ProjectNode, ProjectEdge>,
    sorted_nodes: Vec<petgraph::stable_graph::NodeIndex>,
    nodes_by_path: HashMap<PathBuf, petgraph::stable_graph::NodeIndex>,
    expected_hashes: HashMap<petgraph::stable_graph::NodeIndex, ProjectHash>,
    project_details: HashMap<petgraph::stable_graph::NodeIndex, ProjectNodeDetails>,
    workspaces: HashMap<PathBuf, WorkspaceDefinition>,
}

async fn build_project_graph(
    config: &LoadProjectConfig,
    project_path: &Path,
) -> anyhow::Result<ProjectGraph> {
    let mut graph: petgraph::stable_graph::StableDiGraph<PathBuf, String> =
        petgraph::stable_graph::StableGraph::new();
    let mut expected_hashes = HashMap::new();
    let mut project_details = HashMap::new();
    let mut workspaces = HashMap::new();

    // Use a regex to validate dependency names
    static DEPENDENCY_NAME_REGEX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let dependency_name_regex = DEPENDENCY_NAME_REGEX
        .get_or_init(|| regex::Regex::new("^[a-zA-Z0-9_]+$").expect("failed to compile regex"));

    let mut nodes_by_path = HashMap::new();
    let mut nodes_by_workspace = HashMap::<PathBuf, HashSet<_>>::new();
    let mut queued_paths = VecDeque::from_iter([(
        ResolvedDependency {
            expected_hash: None,
            local_path: project_path.to_owned(),
            locking: config.locking,
            should_lock: None,
        },
        None,
    )]);
    while let Some((resolved_project, referrer)) = queued_paths.pop_front() {
        // Customize the config based on how the project was resolved
        let mut config = config.clone();
        config.locking = resolved_project.locking;

        // Canonicalize the project's path
        let project_path = resolved_project.local_path;
        let project_path = tokio::fs::canonicalize(&project_path)
            .await
            .with_context(|| {
                let referrer_path = referrer
                    .as_ref()
                    .and_then(|(parent_node, _)| graph.node_weight(*parent_node));
                if let Some(referrer_path) = referrer_path {
                    format!(
                        "failed to canonicalize project path {} (referenced from {})",
                        project_path.display(),
                        referrer_path.display()
                    )
                } else {
                    format!(
                        "failed to canonicalize project path {}",
                        project_path.display()
                    )
                }
            })?;

        // Get or insert the node for the resolved project path
        let node = *nodes_by_path
            .entry(project_path.clone())
            .or_insert_with_key(|path| graph.add_node(path.clone()));

        // Record the expected hash if set while resolving (e.g. for
        // registry projects)
        if let Some(expected_hash) = resolved_project.expected_hash {
            let previous_expected_hash = expected_hashes.insert(node, expected_hash);
            let conflicting_expected_hash =
                previous_expected_hash.filter(|hash| *hash != expected_hash);
            if let Some(conflicting_expected_hash) = conflicting_expected_hash {
                anyhow::bail!(
                    "encountered project {} that has conflicting hashes {conflicting_expected_hash} and {expected_hash}",
                    project_path.display(),
                );
            }
        }

        // Insert the project details if not already inserted
        if let std::collections::hash_map::Entry::Vacant(entry) = project_details.entry(node) {
            // Find the workspace, if the project has one
            let workspace = find_workspace(&project_path, &mut workspaces).await?;
            if let Some(workspace) = &workspace {
                nodes_by_workspace
                    .entry(workspace.path.clone())
                    .or_default()
                    .insert(node);
            }

            // Analyze the project
            let project_analysis =
                super::analyze::analyze_project(&config.brioche.vfs, &project_path).await?;

            // Get the current lockfile
            let lockfile_path = project_path.join("brioche.lock");
            let current_lockfile_contents = tokio::fs::read_to_string(&lockfile_path).await;
            let current_lockfile: Option<Lockfile> = match current_lockfile_contents {
                Ok(contents) => match serde_json::from_str(&contents) {
                    Ok(lockfile) => Some(lockfile),
                    Err(error) => match resolved_project.locking {
                        ProjectLocking::Locked => {
                            return Err(error).context(format!(
                                "failed to parse lockfile at {}",
                                lockfile_path.display()
                            ));
                        }
                        ProjectLocking::Unlocked => None,
                    },
                },
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error).context(format!(
                        "failed to read lockfile at {}",
                        lockfile_path.display()
                    ));
                }
            };

            // Use the current lockfile (if present), then start with an empty lockfile
            // to track changes
            let mut lockfile = LockfileState {
                current_lockfile,
                fresh_lockfile: Lockfile::default(),
            };

            // Resolve each dependency, which gives us the local paths to load
            let resolved_dependencies: HashMap<_, _> =
                futures::stream::iter(project_analysis.dependencies())
                    .map(async |(name, dependency_def)| {
                        // Validate the dependency name
                        anyhow::ensure!(
                            dependency_name_regex.is_match(&name),
                            "invalid dependency name"
                        );

                        // Try to resolve the dependency to a local path
                        let resolved_dep_result = resolve_dependency_to_local_path(
                            &config,
                            &project_path,
                            workspace.as_ref(),
                            &name,
                            &dependency_def,
                            lockfile.current_lockfile.as_ref(),
                        )
                        .await;
                        anyhow::Ok((name, resolved_dep_result))
                    })
                    .buffer_unordered(10)
                    .try_collect()
                    .await?;

            let mut dependency_errors = HashMap::new();
            for (name, resolved_dep_result) in resolved_dependencies {
                match resolved_dep_result {
                    Ok(resolved_dep) => {
                        // Update the lockfile if the dependency should be recorded (e.g.
                        // it's from the registry)
                        if let Some(should_lock) = resolved_dep.should_lock {
                            lockfile
                                .fresh_lockfile
                                .dependencies
                                .insert(name.clone(), should_lock);
                        }

                        // Enqueue the resolved dependency to be added
                        // to the graph
                        queued_paths.push_back((resolved_dep, Some((node, name))));
                    }
                    Err(error) => {
                        dependency_errors.insert(name, error);
                    }
                }
            }

            entry.insert(ProjectNodeDetails {
                project_analysis,
                lockfile_path,
                lockfile,
                workspace,
                dependency_errors,
            });
        }

        if let Some((parent_node, dep_name)) = referrer {
            graph.update_edge(parent_node, node, dep_name);
        };
    }

    let mut graph = graph.map(
        |_, path| ProjectNode::Project(path.clone()),
        |_, name| ProjectEdge::DirectDependency { name: name.clone() },
    );

    // Handle graph cycles in a loop. This will finish once the graph
    // is acyclic or if we've found an invalid cycle
    let sorted_nodes = loop {
        let toposort = petgraph::algo::toposort(petgraph::visit::Reversed(&graph), None);
        let cycle = match toposort {
            Ok(sorted_nodes) => {
                // Graph has no cycles, so we're done
                break sorted_nodes;
            }
            Err(cycle) => {
                // Graph has a cycle, so we need to resolve it
                cycle
            }
        };

        let details = project_details
            .get(&cycle.node_id())
            .expect("project node appeared in a cycle but doesn't have any project details");
        let workspace_details = details.workspace.as_ref().and_then(|workspace| {
            let workspace_nodes = nodes_by_workspace.remove(&workspace.path)?;
            Some((workspace.path.clone(), workspace_nodes))
        });
        let Some(workspace_details) = workspace_details else {
            // The cyclic node either isn't part of a workspace, or the
            // cycle extends outside the workspace. In either case, this
            // is an invalid cycle
            let path = graph[cycle.node_id()].path();
            anyhow::bail!("project {} includes a cyclic import", path.display());
        };

        // Sanity check: ensure that the cyclic node is included in the
        // set of workspace nodes
        assert!(
            workspace_details.1.contains(&cycle.node_id()),
            "found a set of workspace nodes, but cyclic node isn't in the set"
        );

        // Add workspace nodes to the graph
        let initialize_workspace_node = graph.add_node(ProjectNode::InitializeWorkspace(
            workspace_details.0.clone(),
        ));
        let finish_workspace_node =
            graph.add_node(ProjectNode::FinishWorkspace(workspace_details.0.clone()));

        for node in &workspace_details.1 {
            // Add an edge so the "workspace finish" node depends on
            // the project node
            graph.add_edge(finish_workspace_node, *node, ProjectEdge::Other);

            // Replace each `DirectDependency` edge within the same workspace
            // with a `WorkspaceDependency` (with the "workspace initialize"
            // node as a target)
            let workspace_edges = graph
                .edges_directed(*node, petgraph::Direction::Outgoing)
                .filter_map(|edge| {
                    if !workspace_details.1.contains(&edge.target()) {
                        return None;
                    }
                    let ProjectEdge::DirectDependency { name } = edge.weight() else {
                        return None;
                    };

                    let path = graph[edge.target()].path();
                    let new_edge = ProjectEdge::WorkspaceDependency {
                        name: name.clone(),
                        path: path.to_path_buf(),
                    };
                    Some((edge.id(), new_edge))
                })
                .collect::<Vec<_>>();
            for (edge_index, new_edge) in workspace_edges {
                graph.remove_edge(edge_index);
                graph.add_edge(*node, initialize_workspace_node, new_edge);
            }
        }
    };

    Ok(ProjectGraph {
        graph,
        sorted_nodes,
        expected_hashes,
        nodes_by_path,
        project_details,
        workspaces,
    })
}

async fn load_project_inner(
    config: &LoadProjectConfig,
    path: &Path,
    depth: u32,
) -> anyhow::Result<(ProjectHash, Arc<Project>, Vec<LoadProjectError>)> {
    tracing::debug!(path = %path.display(), "resolving project");
    let path = tokio::fs::canonicalize(path).await?;

    // Build the project graph
    let mut project_graph = build_project_graph(config, &path).await?;

    let root_node = project_graph.nodes_by_path[&path];

    let mut dirty_lockfiles = HashMap::new();

    let mut nodes_to_projects =
        HashMap::<_, (ProjectHash, ProjectEntry, Vec<LoadProjectError>)>::new();
    let mut workspaces = vec![];

    // Build `Project` and `Workspace` values from the nodes. This is done by
    // iterating through the nodes in a topologically-sorted order so we
    // process each node before its children
    for node in project_graph.sorted_nodes {
        match &project_graph.graph[node] {
            ProjectNode::Project(path) => {
                let mut details = project_graph
                    .project_details
                    .remove(&node)
                    .expect("details not found for project graph node");
                let mut errors = vec![];

                for (name, error) in details.dependency_errors {
                    errors.push(LoadProjectError::FailedToResolveDependency {
                        name,
                        cause: format!("{error:#}"),
                    });
                }

                let mut statics = HashMap::new();
                for module in details.project_analysis.local_modules.values() {
                    let mut module_statics = BTreeMap::new();
                    for static_ in &module.statics {
                        // Only resolve the static if we need a fully valid project
                        let resolved_static = match config.validation {
                            ProjectValidation::Standard => {
                                let resolved_static = resolve_static(
                                    &config.brioche,
                                    path,
                                    module,
                                    static_,
                                    config.locking,
                                    &mut details.lockfile,
                                )
                                .await;
                                Some(resolved_static)
                            }
                            ProjectValidation::Minimal => None,
                        };

                        match resolved_static {
                            Some(Ok(static_output)) => {
                                module_statics.insert(static_.clone(), Some(static_output));
                            }
                            Some(Err(error)) => {
                                module_statics.insert(static_.clone(), None);
                                errors.push(LoadProjectError::FailedToLoadStatic {
                                    static_query: static_.clone(),
                                    cause: format!("{error:#}"),
                                });
                            }
                            None => {
                                module_statics.insert(static_.clone(), None);
                            }
                        }
                    }

                    if !module_statics.is_empty() {
                        statics.insert(module.project_subpath.clone(), module_statics);
                    }
                }

                let mut dependencies = HashMap::new();
                let edges = project_graph
                    .graph
                    .edges_directed(node, petgraph::Direction::Outgoing);
                for edge in edges {
                    match edge.weight() {
                        ProjectEdge::DirectDependency { name: dep_name } => {
                            let (dep_project_hash, _, dep_errors) =
                                &nodes_to_projects[&edge.target()];
                            dependencies.insert(
                                dep_name.clone(),
                                DependencyRef::Project(*dep_project_hash),
                            );

                            for error in &dep_errors[..] {
                                errors.push(LoadProjectError::DependencyError {
                                    name: dep_name.clone(),
                                    error: Box::new(error.clone()),
                                });
                            }
                        }
                        ProjectEdge::WorkspaceDependency {
                            name: dep_name,
                            path: dep_path,
                        } => {
                            let workspace = details.workspace.as_ref().expect(
                                "node has WorkspaceDependency edge, but details.workspace is None",
                            );
                            let dep_member_path = dep_path.relative_to(&workspace.path).with_context(|| {
                                    format!("failed to resolve dependency {dep_name:?} at {} relative to workspace path {}", dep_path.display(), workspace.path.display())
                            })?;
                            dependencies.insert(
                                dep_name.clone(),
                                DependencyRef::WorkspaceMember {
                                    path: dep_member_path,
                                },
                            );
                        }
                        ProjectEdge::Other { .. } => {}
                    }
                }

                let modules = details
                    .project_analysis
                    .local_modules
                    .values()
                    .map(|module| (module.project_subpath.clone(), module.file_id))
                    .collect();

                let project = Arc::new(Project {
                    definition: details.project_analysis.definition,
                    dependencies,
                    modules,
                    statics,
                });
                let project_hash = ProjectHash::from_serializable(&project)?;

                // If the lockfile doesn't need to be fully valid, ensure that
                // the new lockfile includes old statics and dependencies that
                // weren't updated. This can mean that e.g. unnecessary downloads
                // and dependencies are kept, but this is appropriate for
                // situations like the LSP
                match config.validation {
                    ProjectValidation::Standard => {}
                    ProjectValidation::Minimal => {
                        let Lockfile {
                            dependencies: new_dependencies,
                            downloads: new_downloads,
                            git_refs: new_git_refs,
                        } = &mut details.lockfile.fresh_lockfile;

                        if let Some(lockfile) = &details.lockfile.current_lockfile {
                            for (name, hash) in &lockfile.dependencies {
                                new_dependencies.entry(name.clone()).or_insert(*hash);
                            }

                            for (url, hash) in &lockfile.downloads {
                                new_downloads
                                    .entry(url.clone())
                                    .or_insert_with(|| hash.clone());
                            }

                            for (url, options) in &lockfile.git_refs {
                                new_git_refs
                                    .entry(url.clone())
                                    .or_insert_with(|| options.clone());
                            }
                        }
                    }
                }

                match (details.lockfile.is_up_to_date(), config.locking) {
                    (true, _) => {
                        // Lockfile is up-to-date
                    }
                    (false, ProjectLocking::Unlocked) => {
                        // Lockfile is out of date but we're loading it "unlocked",
                        // so record the lockfile as dirty

                        dirty_lockfiles
                            .insert(details.lockfile_path, details.lockfile.fresh_lockfile);
                    }
                    (false, ProjectLocking::Locked) => {
                        // Lockfile is out of date and we're loading in "locked"
                        // mode, meaning we expected it to be up-to-date

                        anyhow::bail!(
                            "lockfile at {} is out of date",
                            details.lockfile_path.display()
                        );
                    }
                }

                if !errors.is_empty() {
                    tracing::debug!(?path, ?errors, "project loaded with errors");
                }
                nodes_to_projects
                    .insert(node, (project_hash, ProjectEntry::Project(project), errors));
            }
            ProjectNode::InitializeWorkspace(_) => {}
            ProjectNode::FinishWorkspace(workspace_path) => {
                let project_nodes: Vec<_> = project_graph
                    .graph
                    .edges_directed(node, petgraph::Direction::Outgoing)
                    .map(|edge| edge.target())
                    .collect();

                let mut members = BTreeMap::new();
                let mut member_nodes = vec![];
                let mut member_load_errors = vec![];
                for node in project_nodes {
                    let project_path = project_graph.graph[node].path();
                    anyhow::ensure!(
                        project_path.starts_with(workspace_path),
                        "expected project {} to be a child of workspace {}",
                        project_path.display(),
                        workspace_path.display()
                    );
                    let workspace_member_path = project_path
                        .relative_to(workspace_path)
                        .expect("failed to get project path relative to workspace path");

                    let (_, project_entry, load_project_errors) = nodes_to_projects
                        .remove(&node)
                        .expect("nodes_to_projects did not contain project for workspace member");
                    let ProjectEntry::Project(project) = project_entry else {
                        panic!(
                            "nodes_to_project contained an invalid project entry for workspace member"
                        );
                    };

                    member_load_errors.extend(
                        load_project_errors
                            .into_iter()
                            .map(|error| (workspace_member_path.clone(), error)),
                    );

                    members.insert(workspace_member_path.clone(), project);
                    member_nodes.push((node, workspace_member_path));
                }

                let workspace = Workspace { members };
                let workspace_hash = WorkspaceHash::from_serializable(&workspace)?;

                workspaces.push((workspace_hash, Arc::new(workspace)));

                for (node, workspace_member_path) in member_nodes {
                    let load_project_errors = member_load_errors
                        .iter()
                        .map(|(error_member_path, error)| {
                            if *error_member_path == workspace_member_path {
                                error.clone()
                            } else {
                                LoadProjectError::WorkspaceMemberError {
                                    workspace_member_path: error_member_path.clone(),
                                    error: Box::new(error.clone()),
                                }
                            }
                        })
                        .collect();

                    let project_entry = ProjectEntry::WorkspaceMember {
                        workspace: workspace_hash,
                        path: workspace_member_path,
                    };
                    let project_hash = ProjectHash::from_serializable(&project_entry)?;
                    nodes_to_projects
                        .insert(node, (project_hash, project_entry, load_project_errors));
                }
            }
        };
    }

    let &(project_hash, _, _) = nodes_to_projects
        .get(&root_node)
        .expect("results for root project not found");

    let mut projects = config
        .projects
        .inner
        .write()
        .expect("failed to acquire 'projects' lock");

    projects.dirty_lockfiles.extend(dirty_lockfiles);

    projects.workspaces.extend(workspaces);

    for (node, (project_hash, project_entry, mut errors)) in nodes_to_projects {
        let path = project_graph.graph[node].path();

        if let Some(expected_hash) = project_graph.expected_hashes.remove(&node) {
            if expected_hash != project_hash {
                errors.push(LoadProjectError::InvalidProjectHash {
                    path: project_graph.graph[node].path().to_path_buf(),
                    expected_hash: expected_hash.to_string(),
                    actual_hash: project_hash.to_string(),
                });
            }
        }

        projects.projects.insert(project_hash, project_entry);
        projects
            .paths_to_projects
            .insert(path.to_path_buf(), project_hash);
        projects
            .projects_to_paths
            .entry(project_hash)
            .or_default()
            .insert(path.to_path_buf());
        projects.project_load_errors.insert(project_hash, errors);
    }

    let project = projects
        .project(project_hash)
        .expect("root project not found");
    let errors = projects.project_load_errors[&project_hash].clone();
    Ok((project_hash, project.clone(), errors))
}

struct LockfileState {
    current_lockfile: Option<Lockfile>,
    fresh_lockfile: Lockfile,
}

impl LockfileState {
    fn is_up_to_date(&self) -> bool {
        self.current_lockfile.as_ref() == Some(&self.fresh_lockfile)
    }
}

#[derive(Clone)]
struct LoadProjectConfig {
    projects: Projects,
    brioche: Brioche,
    validation: ProjectValidation,
    locking: ProjectLocking,
}

async fn resolve_dependency_to_local_path(
    config: &LoadProjectConfig,
    project_path: &Path,
    workspace: Option<&WorkspaceInfo>,
    dependency_name: &str,
    dependency_definition: &DependencyDefinition,
    current_lockfile: Option<&Lockfile>,
) -> anyhow::Result<ResolvedDependency> {
    match dependency_definition {
        DependencyDefinition::Path { path } => {
            let local_path = project_path.join(path);
            Ok(ResolvedDependency {
                expected_hash: None,
                local_path,
                locking: config.locking,
                should_lock: None,
            })
        }
        DependencyDefinition::Version(version) => {
            let resolved = resolve_dependency_version_to_local_path(
                config,
                workspace,
                dependency_name,
                version,
                current_lockfile,
            )
            .await?;
            Ok(resolved)
        }
    }
}

async fn resolve_dependency_version_to_local_path(
    config: &LoadProjectConfig,
    workspace: Option<&WorkspaceInfo>,
    dependency_name: &str,
    dependency_version: &Version,
    current_lockfile: Option<&Lockfile>,
) -> anyhow::Result<ResolvedDependency> {
    if let Some(workspace) = workspace {
        if let Some(workspace_path) =
            resolve_workspace_project_path(workspace, dependency_name).await?
        {
            // Eventually, we'll validate that the version of the project
            // from the workspace matches the requested dependency version
            match dependency_version {
                Version::Any => {}
            }

            return Ok(ResolvedDependency {
                local_path: workspace_path,
                expected_hash: None,
                locking: config.locking,
                should_lock: None,
            });
        }
    }

    // TODO: Validate that the requested dependency version matches the
    // version in the lockfile
    let lockfile_dep_hash =
        current_lockfile.and_then(|lockfile| lockfile.dependencies.get(dependency_name));
    let dep_hash = match lockfile_dep_hash {
        Some(dep_hash) => *dep_hash,
        None => match config.locking {
            ProjectLocking::Unlocked => {
                resolve_project_from_registry(&config.brioche, dependency_name, dependency_version)
                    .await
                    .with_context(|| {
                        format!("failed to resolve '{dependency_name}' from registry")
                    })?
            }
            ProjectLocking::Locked => {
                anyhow::bail!("dependency '{}' not found in lockfile", dependency_name);
            }
        },
    };

    let local_path = fetch_project_from_cache(
        &config.projects,
        &config.brioche,
        dep_hash,
        config.validation,
    )
    .await
    .with_context(|| format!("failed to fetch '{dependency_name}' from cache"))?;

    Ok(ResolvedDependency {
        local_path,
        expected_hash: Some(dep_hash),
        locking: ProjectLocking::Locked,
        should_lock: Some(dep_hash),
    })
}

struct ResolvedDependency {
    local_path: PathBuf,
    expected_hash: Option<ProjectHash>,
    locking: ProjectLocking,
    should_lock: Option<ProjectHash>,
}

pub async fn resolve_project_from_registry(
    brioche: &Brioche,
    dependency_name: &str,
    dependency_version: &Version,
) -> anyhow::Result<ProjectHash> {
    let tag = match dependency_version {
        Version::Any => "latest",
    };
    let response = brioche
        .registry_client
        .get_project_tag(dependency_name, tag)
        .await?;
    Ok(response.project_hash)
}

pub async fn fetch_project_from_cache(
    projects: &Projects,
    brioche: &Brioche,
    project_hash: ProjectHash,
    validation: ProjectValidation,
) -> anyhow::Result<PathBuf> {
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

    let local_path = brioche
        .data_dir
        .join("projects")
        .join(project_hash.to_string());

    match validation {
        ProjectValidation::Standard => {
            let local_project_hash = local_project_hash(brioche, &local_path).await?;
            if local_project_hash == Some(project_hash) {
                // Local project hash matches, so no need to fetch
                return Ok(local_path);
            }
        }
        ProjectValidation::Minimal => {
            let local_project_exists = tokio::fs::try_exists(&local_path).await?;
            if local_project_exists {
                // Directory for the local project exists. No need to fetch,
                // and no need to validate since validation wasn't requested
                return Ok(local_path);
            }
        }
    }

    // By this point, we know the project doesn't exist locally so we
    // need to fetch it.

    let project_artifact_hash = crate::cache::load_project_artifact_hash(brioche, project_hash)
        .await?
        .with_context(|| format!("project with hash {project_hash} not found in cache"))?;
    let project_artifact = crate::cache::load_artifact(
        brioche,
        project_artifact_hash,
        crate::reporter::job::CacheFetchKind::Project,
    )
    .await?
    .with_context(|| {
        format!("artifact {project_artifact_hash} for project {project_hash} not found in cache")
    })?;
    let Artifact::Directory(project_artifact) = project_artifact else {
        anyhow::bail!("expected artifact from cache for project {project_hash} to be a directory");
    };

    let saved_projects =
        super::artifact::save_projects_from_artifact(brioche, projects, &project_artifact).await?;
    anyhow::ensure!(
        saved_projects.contains(&project_hash),
        "artifact for project found in cache, but it did not contain the project {project_hash}"
    );

    Ok(local_path)
}

async fn resolve_workspace_project_path(
    workspace: &WorkspaceInfo,
    project_name: &str,
) -> anyhow::Result<Option<PathBuf>> {
    for member in &workspace.definition.members {
        match member {
            WorkspaceMember::Path(path, name) => {
                if name == project_name {
                    let dep_path = path.join(name).to_logical_path(&workspace.path);
                    anyhow::ensure!(
                        tokio::fs::try_exists(&dep_path).await?,
                        "workspace member does not exist: {}",
                        dep_path.display()
                    );
                    return Ok(Some(dep_path));
                }
            }
            WorkspaceMember::WildcardPath(path) => {
                let dep_path = path.join(project_name).to_logical_path(&workspace.path);
                if tokio::fs::try_exists(&dep_path).await? {
                    return Ok(Some(dep_path));
                }
            }
        }
    }

    Ok(None)
}

async fn find_workspace(
    project_path: &Path,
    workspaces: &mut HashMap<PathBuf, WorkspaceDefinition>,
) -> anyhow::Result<Option<WorkspaceInfo>> {
    for workspace_path in project_path.ancestors().skip(1) {
        let entry = match workspaces.entry(workspace_path.to_path_buf()) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                // Workspace already loaded with the given path
                return Ok(Some(WorkspaceInfo {
                    path: entry.key().clone(),
                    definition: entry.get().clone(),
                }));
            }
            std::collections::hash_map::Entry::Vacant(entry) => entry,
        };

        // Try to read the workspace definition
        let workspace_def_path = workspace_path.join("brioche_workspace.toml");
        let workspace_def_contents = tokio::fs::read_to_string(&workspace_def_path).await;
        let workspace_def_contents = match workspace_def_contents {
            Ok(workspace_def) => workspace_def,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Workspace file not found, keep searching
                continue;
            }
            Err(error) => {
                // Failed to read workspace file
                return Err(error).with_context(|| {
                    format!(
                        "failed to read workspace file {}",
                        workspace_def_path.display()
                    )
                })?;
            }
        };

        // Parse the workspace definition
        let workspace_def: WorkspaceDefinition = toml::from_str(&workspace_def_contents)
            .with_context(|| {
                format!(
                    "failed to parse workspace file {}",
                    workspace_def_path.display()
                )
            })?;

        // Insert the workspace definition
        entry.insert_entry(workspace_def.clone());
        return Ok(Some(WorkspaceInfo {
            path: workspace_path.to_path_buf(),
            definition: workspace_def,
        }));
    }

    Ok(None)
}

#[derive(Debug, Clone)]
struct WorkspaceInfo {
    definition: WorkspaceDefinition,
    path: PathBuf,
}

async fn resolve_static(
    brioche: &Brioche,
    project_root: &Path,
    module: &super::analyze::ModuleAnalysis,
    static_: &StaticQuery,
    locking: ProjectLocking,
    lockfile: &mut LockfileState,
) -> anyhow::Result<StaticOutput> {
    match static_ {
        StaticQuery::Include(include) => {
            let module_path = module.project_subpath.to_path(project_root);
            let module_dir_path = module_path
                .parent()
                .context("no parent path for module path")?;
            let input_path = module_dir_path.join(include.path());

            let canonical_project_root =
                tokio::fs::canonicalize(project_root)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to canonicalize project root {}",
                            project_root.display()
                        )
                    })?;
            let canonical_input_path =
                tokio::fs::canonicalize(&input_path)
                    .await
                    .with_context(|| {
                        format!("failed to canonicalize input path {}", input_path.display())
                    })?;
            anyhow::ensure!(
                canonical_input_path.starts_with(&canonical_project_root),
                "input path {} escapes project root {}",
                include.path(),
                project_root.display(),
            );

            let artifact = crate::input::create_input(
                brioche,
                crate::input::InputOptions {
                    input_path: &input_path,
                    remove_input: false,
                    resource_dir: None,
                    input_resource_dirs: &[],
                    saved_paths: &mut Default::default(),
                    meta: &Default::default(),
                },
            )
            .await?;
            match (&include, &artifact.value) {
                (super::analyze::StaticInclude::File { .. }, Artifact::File(_))
                | (super::analyze::StaticInclude::Directory { .. }, Artifact::Directory(_)) => {
                    // Valid
                }
                (super::analyze::StaticInclude::File { path }, _) => {
                    anyhow::bail!("expected path {path:?} to be a file");
                }
                (super::analyze::StaticInclude::Directory { path }, _) => {
                    anyhow::bail!("expected path {path:?} to be a directory");
                }
            }
            let artifact_hash = artifact.hash();

            let recipe = crate::recipe::Recipe::from(artifact.value);
            crate::recipe::save_recipes(brioche, [&recipe]).await?;

            Ok(StaticOutput::RecipeHash(artifact_hash))
        }
        StaticQuery::Glob { patterns } => {
            let module_path = module.project_subpath.to_path(project_root);
            let module_dir_path = module_path
                .parent()
                .context("no parent path for module path")?;

            let mut glob_set = globset::GlobSetBuilder::new();
            for pattern in patterns {
                let glob = globset::GlobBuilder::new(pattern)
                    .case_insensitive(false)
                    .literal_separator(true)
                    .backslash_escape(true)
                    .empty_alternates(true)
                    .build()?;
                glob_set.add(glob);
            }
            let glob_set = glob_set.build()?;

            let paths = tokio::task::spawn_blocking({
                let module_dir_path = module_dir_path.to_owned();
                move || {
                    let mut paths = vec![];
                    for entry in walkdir::WalkDir::new(&module_dir_path) {
                        let entry =
                            entry.context("failed to get directory entry while matching globs")?;
                        let relative_entry_path = pathdiff::diff_paths(
                            entry.path(),
                            &module_dir_path,
                        )
                        .with_context(|| {
                            format!(
                                "failed to resolve matched path {} relative to module path {}",
                                entry.path().display(),
                                module_dir_path.display(),
                            )
                        })?;
                        if glob_set.is_match(&relative_entry_path) {
                            paths.push((entry.path().to_owned(), relative_entry_path));
                        }
                    }

                    anyhow::Ok(paths)
                }
            })
            .await??;

            let artifacts = futures::stream::iter(paths)
                .then(|(full_path, relative_path)| async move {
                    let artifact = crate::input::create_input(
                        brioche,
                        crate::input::InputOptions {
                            input_path: &full_path,
                            remove_input: false,
                            resource_dir: None,
                            input_resource_dirs: &[],
                            saved_paths: &mut Default::default(),
                            meta: &Default::default(),
                        },
                    )
                    .await?;
                    anyhow::Ok((relative_path, artifact.value))
                })
                .try_collect::<Vec<_>>()
                .await?;

            let mut directory = crate::recipe::Directory::default();
            for (path, artifact) in artifacts {
                let path = <Vec<u8> as bstr::ByteVec>::from_os_string(path.as_os_str().to_owned())
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "invalid path name {} that matched glob pattern",
                            path.display()
                        )
                    })?;
                directory.insert(brioche, &path, Some(artifact)).await?;
            }

            let recipe = crate::recipe::Recipe::from(directory);
            let recipe_hash = recipe.hash();

            crate::recipe::save_recipes(brioche, [&recipe]).await?;

            Ok(StaticOutput::RecipeHash(recipe_hash))
        }
        StaticQuery::Download { url } => {
            let current_download_hash = lockfile
                .current_lockfile
                .as_ref()
                .and_then(|lockfile| lockfile.downloads.get(url));

            let download_hash: crate::Hash;
            let blob_hash: Option<crate::blob::BlobHash>;

            match (current_download_hash, locking) {
                (Some(hash), _) => {
                    // If we have the hash from the lockfile, use it to build
                    // the recipe. But, we don't have the blob hash yet
                    download_hash = hash.clone();
                    blob_hash = None;
                }
                (None, ProjectLocking::Unlocked) => {
                    // Download the URL as a blob
                    let new_blob_hash = crate::download::download(brioche, url, None).await?;
                    let blob_path = crate::blob::local_blob_path(brioche, new_blob_hash);
                    let mut blob = tokio::fs::File::open(&blob_path).await?;

                    // Compute a hash to store in the lockfile
                    let mut hasher = crate::Hasher::new_sha256();
                    let mut buffer = vec![0u8; 1024 * 1024];
                    loop {
                        let length = blob
                            .read(&mut buffer)
                            .await
                            .context("failed to read blob")?;
                        if length == 0 {
                            break;
                        }

                        hasher.update(&buffer[..length]);
                    }

                    // Record both the hash for the recipe plus the output
                    // blob hash
                    download_hash = hasher.finish()?;
                    blob_hash = Some(new_blob_hash);
                }
                (None, ProjectLocking::Locked) => {
                    // Error out if the download isn't in the lockfile but where
                    // updating the lockfile is disabled
                    anyhow::bail!("hash for download '{url}' not found in lockfile");
                }
            }

            // Create the download recipe, which is equivalent to the URL
            // we downloaded or the one recorded in the lockfile
            let download_recipe = crate::recipe::Recipe::Download(crate::recipe::DownloadRecipe {
                hash: download_hash.clone(),
                url: url.clone(),
            });
            let download_recipe_hash = download_recipe.hash();

            if let Some(blob_hash) = blob_hash {
                // If we downloaded the blob, save the recipe and the output
                // artifact. This effectively caches the download

                let download_artifact = crate::recipe::Artifact::File(crate::recipe::File {
                    content_blob: blob_hash,
                    executable: false,
                    resources: Default::default(),
                });

                let download_recipe_json = serde_json::to_string(&download_recipe)
                    .context("failed to serialize download recipe")?;
                let download_artifact_hash = download_artifact.hash();
                let download_artifact_json = serde_json::to_string(&download_artifact)
                    .context("failed to serialize download output artifact")?;
                crate::bake::save_bake_result(
                    brioche,
                    download_recipe_hash,
                    &download_recipe_json,
                    download_artifact_hash,
                    &download_artifact_json,
                )
                .await?;
            } else {
                // If we didn't download the blob, just save the recipe. This
                // either means we've already cached the download before,
                // or we haven't and we'll need to download it or fetch
                // it from the registry
                crate::recipe::save_recipes(brioche, &[download_recipe]).await?;
            }

            // Update the new lockfile with the download hash
            lockfile
                .fresh_lockfile
                .downloads
                .insert(url.clone(), download_hash.clone());

            Ok(StaticOutput::Kind(StaticOutputKind::Download {
                hash: download_hash,
            }))
        }
        StaticQuery::GitRef(GitRefOptions { repository, ref_ }) => {
            let current_commit = lockfile.current_lockfile.as_ref().and_then(|lockfile| {
                lockfile
                    .git_refs
                    .get(repository)
                    .and_then(|repo_refs| repo_refs.get(ref_))
            });

            let commit = match (current_commit, locking) {
                (Some(commit), _) => commit.clone(),
                (None, ProjectLocking::Unlocked) => {
                    // Fetch the current commit hash of the git ref from the repo
                    crate::download::fetch_git_commit_for_ref(repository, ref_)
                        .await
                        .with_context(|| {
                            format!("failed to fetch ref '{ref_}' from git repo '{repository}'")
                        })?
                }
                (None, ProjectLocking::Locked) => {
                    // Error out if the git ref isn't in the lockfile but where
                    // updating the lockfile is disabled
                    anyhow::bail!(
                        "commit for git repo '{repository}' ref '{ref_}' not found in lockfile"
                    );
                }
            };

            // Update the new lockfile with the commit
            let repo_refs = lockfile
                .fresh_lockfile
                .git_refs
                .entry(repository.clone())
                .or_default();
            repo_refs.insert(ref_.clone(), commit.clone());

            Ok(StaticOutput::Kind(StaticOutputKind::GitRef { commit }))
        }
    }
}

// TODO: This should be refactored to share code with `load_project_inner`
pub async fn local_project_hash(
    brioche: &Brioche,
    path: &Path,
) -> anyhow::Result<Option<ProjectHash>> {
    let real_path = tokio::fs::canonicalize(path).await;
    let path = match real_path {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => {
            return Err(error).context(format!("failed to canonicalize path {}", path.display()));
        }
    };

    let project_analysis = super::analyze::analyze_project(&brioche.vfs, &path).await;
    let Ok(project_analysis) = project_analysis else {
        return Ok(None);
    };

    let lockfile_path = path.join("brioche.lock");
    let lockfile_contents = tokio::fs::read_to_string(&lockfile_path).await;
    let lockfile: Lockfile = match lockfile_contents {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(lockfile) => lockfile,
            Err(_) => {
                return Ok(None);
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => {
            return Err(error).context(format!(
                "failed to read lockfile at {}",
                lockfile_path.display()
            ));
        }
    };

    let modules = project_analysis
        .local_modules
        .values()
        .map(|module| (module.project_subpath.clone(), module.file_id))
        .collect();
    let mut statics = HashMap::new();
    for module in project_analysis.local_modules.values() {
        let mut module_statics = BTreeMap::new();
        for static_ in &module.statics {
            let static_output = resolve_static(
                brioche,
                &path,
                module,
                static_,
                ProjectLocking::Locked,
                &mut LockfileState {
                    current_lockfile: Some(lockfile.clone()),
                    fresh_lockfile: Lockfile::default(),
                },
            )
            .await?;
            module_statics.insert(static_.clone(), Some(static_output));
        }

        if !module_statics.is_empty() {
            statics.insert(module.project_subpath.clone(), module_statics);
        }
    }

    let dependencies = lockfile
        .dependencies
        .into_iter()
        .map(|(dep_name, dep_hash)| (dep_name, DependencyRef::Project(dep_hash)))
        .collect();
    let project = Project {
        definition: project_analysis.definition,
        dependencies,
        modules,
        statics,
    };
    let project_hash = ProjectHash::from_serializable(&project)?;

    Ok(Some(project_hash))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoadProjectError {
    FailedToResolveDependency {
        name: String,
        cause: String,
    },
    FailedToLoadDependency {
        name: String,
        cause: String,
    },
    FailedToLoadStatic {
        static_query: StaticQuery,
        cause: String,
    },
    DependencyError {
        name: String,
        error: Box<LoadProjectError>,
    },
    WorkspaceMemberError {
        workspace_member_path: RelativePathBuf,
        error: Box<LoadProjectError>,
    },
    InvalidProjectHash {
        path: PathBuf,
        expected_hash: String,
        actual_hash: String,
    },
}
