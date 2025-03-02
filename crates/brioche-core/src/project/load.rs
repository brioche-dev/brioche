use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use tokio::io::AsyncReadExt as _;

use crate::{Brioche, recipe::Artifact};

use super::{
    DependencyDefinition, Lockfile, Project, ProjectHash, ProjectLocking, ProjectValidation,
    Projects, Version, Workspace, WorkspaceMember,
    analyze::{GitRefOptions, StaticOutput, StaticOutputKind, StaticQuery},
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

async fn load_project_inner(
    config: &LoadProjectConfig,
    path: &Path,
    depth: u32,
) -> anyhow::Result<(ProjectHash, Arc<Project>, Vec<LoadProjectError>)> {
    tracing::debug!(path = %path.display(), "resolving project");

    let path = tokio::fs::canonicalize(path)
        .await
        .with_context(|| format!("failed to canonicalize path {}", path.display()))?;

    // Find the workspace, if the project has one
    let workspace = find_workspace(&path).await?;

    let project_analysis = super::analyze::analyze_project(&config.brioche.vfs, &path).await?;

    // Get the current lockfile
    let lockfile_path = path.join("brioche.lock");
    let current_lockfile_contents = tokio::fs::read_to_string(&lockfile_path).await;
    let current_lockfile: Option<Lockfile> = match current_lockfile_contents {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(lockfile) => Some(lockfile),
            Err(error) => match config.locking {
                ProjectLocking::Locked => {
                    return Err(error).context(format!(
                        "failed to parse lockfile at {}",
                        lockfile_path.display()
                    ));
                }
                ProjectLocking::Unlocked => None,
            },
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => match config.locking {
            ProjectLocking::Locked => {
                anyhow::bail!("lockfile not found: {}", lockfile_path.display());
            }
            ProjectLocking::Unlocked => None,
        },
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

    let mut errors = vec![];

    // Ensure that the currently-loaded project hasn't exceeded the max depth
    let dep_depth = depth
        .checked_sub(1)
        .context("project dependency depth exceeded")?;

    // Use a regex to validate dependency names
    static DEPENDENCY_NAME_REGEX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let dependency_name_regex = DEPENDENCY_NAME_REGEX
        .get_or_init(|| regex::Regex::new("^[a-zA-Z0-9_]+$").expect("failed to compile regex"));

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
                    config,
                    &path,
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

    // Load each dependency
    let mut dependencies = HashMap::new();
    for (name, resolved_dep_result) in resolved_dependencies {
        let resolved_dep = match resolved_dep_result {
            Ok(resolved) => resolved,
            Err(error) => {
                // Dependency failed to resolve, so add it to the list
                // of errors
                errors.push(LoadProjectError::FailedToResolveDependency {
                    name,
                    cause: format!("{error:#}"),
                });
                continue;
            }
        };

        let mut resolved_dep_config = config.clone();
        resolved_dep_config.locking = resolved_dep.locking;
        let result = Box::pin(load_project_inner(
            &resolved_dep_config,
            &resolved_dep.local_path,
            dep_depth,
        ))
        .await;
        let (dep_hash, _, dep_errors) = match result {
            Ok(dep) => dep,
            Err(error) => {
                // Failed to load resolved dependency, add it to the list
                // of errors
                errors.push(LoadProjectError::FailedToLoadDependency {
                    name,
                    cause: format!("{error:#}"),
                });
                continue;
            }
        };

        if let Some(expected_hash) = resolved_dep.expected_hash {
            if expected_hash != dep_hash {
                // The resolved dependency locally has a different hash
                // than it should (e.g. a project we downloaded from the cache)
                errors.push(LoadProjectError::FailedToLoadDependency {
                    name: name.clone(),
                    cause: format!(
                        "resolved dependency at '{}' did not match expected hash",
                        resolved_dep.local_path.display()
                    ),
                });
            }
        }

        // Add the errors returned while loading the dependency, if any
        errors.extend(
            dep_errors
                .into_iter()
                .map(|error| LoadProjectError::DependencyError {
                    name: name.clone(),
                    error: Box::new(error),
                }),
        );

        // Update the lockfile if the dependency should be recorded (e.g.
        // it's from the registry)
        if let Some(should_lock) = resolved_dep.should_lock {
            lockfile
                .fresh_lockfile
                .dependencies
                .insert(name.clone(), should_lock);
        }

        dependencies.insert(name, dep_hash);
    }

    // Load the statics from all of the project's modules
    let modules = project_analysis
        .local_modules
        .values()
        .map(|module| (module.project_subpath.clone(), module.file_id))
        .collect();
    let mut statics = HashMap::new();
    for module in project_analysis.local_modules.values() {
        let mut module_statics = BTreeMap::new();
        for static_ in &module.statics {
            // Only resolve the static if we need a fully valid project
            match config.validation {
                ProjectValidation::Standard => {
                    let recipe_hash = resolve_static(
                        &config.brioche,
                        &path,
                        module,
                        static_,
                        config.locking,
                        &mut lockfile,
                    )
                    .await?;
                    module_statics.insert(static_.clone(), Some(recipe_hash));
                }
                ProjectValidation::Minimal => {
                    module_statics.insert(static_.clone(), None);
                }
            }
        }

        if !module_statics.is_empty() {
            statics.insert(module.project_subpath.clone(), module_statics);
        }
    }

    // Build the project from its components and compute the project hash
    let project = Project {
        definition: project_analysis.definition,
        dependencies,
        modules,
        statics,
    };
    let project = Arc::new(project);
    let project_hash = ProjectHash::from_serializable(&project)?;

    if !errors.is_empty() {
        tracing::debug!(?path, ?errors, "project loaded with errors");
    }
    {
        let mut projects = config
            .projects
            .inner
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;

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
                } = &mut lockfile.fresh_lockfile;

                if let Some(lockfile) = &lockfile.current_lockfile {
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

        match (lockfile.is_up_to_date(), config.locking) {
            (true, _) => {
                // Lockfile is up-to-date
            }
            (false, ProjectLocking::Unlocked) => {
                // Lockfile is out of date but we're loading it "unlocked",
                // so record the lockfile as dirty

                projects
                    .dirty_lockfiles
                    .insert(lockfile_path, lockfile.fresh_lockfile);
            }
            (false, ProjectLocking::Locked) => {
                // Lockfile is out of date and we're loading in "locked"
                // mode, meaning we expected it to be up-to-date

                anyhow::bail!("lockfile at {} is out of date", lockfile_path.display());
            }
        }

        // Insert the new project
        projects.projects.insert(project_hash, project.clone());
        projects
            .paths_to_projects
            .insert(path.clone(), project_hash);
        projects
            .projects_to_paths
            .entry(project_hash)
            .or_default()
            .insert(path);
        projects
            .project_load_errors
            .insert(project_hash, errors.clone());
    }

    Ok((project_hash, project, errors))
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
    workspace: Option<&Workspace>,
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
    workspace: Option<&Workspace>,
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
    workspace: &Workspace,
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

async fn find_workspace(project_path: &Path) -> anyhow::Result<Option<Workspace>> {
    for workspace_path in project_path.ancestors().skip(1) {
        let workspace_def_path = workspace_path.join("brioche_workspace.toml");
        if tokio::fs::try_exists(&workspace_def_path).await? {
            let workspace_def = tokio::fs::read_to_string(&workspace_def_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to read workspace file {}",
                        workspace_def_path.display()
                    )
                })?;
            let workspace_def = toml::from_str(&workspace_def).with_context(|| {
                format!(
                    "failed to parse workspace file {}",
                    workspace_def_path.display()
                )
            })?;
            return Ok(Some(Workspace {
                definition: workspace_def,
                path: workspace_path.to_owned(),
            }));
        }
    }

    Ok(None)
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

    let dependencies = lockfile.dependencies.into_iter().collect();
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
    DependencyError {
        name: String,
        error: Box<LoadProjectError>,
    },
}
