use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use analyze::StaticQuery;
use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use relative_path::{PathExt as _, RelativePath, RelativePathBuf};
use tokio::io::AsyncWriteExt as _;

use crate::recipe::{Artifact, RecipeHash};

use super::{vfs::FileId, Brioche};

pub mod analyze;

#[derive(Clone, Default)]
pub struct Projects {
    inner: Arc<std::sync::RwLock<ProjectsInner>>,
}

impl Projects {
    pub async fn load(
        &self,
        brioche: &Brioche,
        path: &Path,
        fully_valid: bool,
    ) -> anyhow::Result<ProjectHash> {
        {
            let projects = self
                .inner
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
            if let Some(project_hash) = projects.paths_to_projects.get(path) {
                if fully_valid {
                    let errors = &projects.project_load_errors[project_hash];
                    if !errors.is_empty() {
                        anyhow::bail!("project load errors: {errors:?}");
                    }
                }

                return Ok(*project_hash);
            }
        }

        let project_hash = load_project(
            self.clone(),
            brioche.clone(),
            path.to_owned(),
            fully_valid,
            100,
        )
        .await?;

        if fully_valid {
            let projects = self
                .inner
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
            let errors = &projects.project_load_errors[&project_hash];
            if !errors.is_empty() {
                anyhow::bail!("project load errors: {errors:?}");
            }
        }

        Ok(project_hash)
    }

    pub async fn load_from_module_path(
        &self,
        brioche: &Brioche,
        path: &Path,
        validate: bool,
    ) -> anyhow::Result<ProjectHash> {
        {
            let projects = self
                .inner
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
            if let Some(project_hash) = projects.find_containing_project(path) {
                return Ok(project_hash);
            }
        }

        for ancestor in path.ancestors().skip(1) {
            if tokio::fs::try_exists(ancestor.join("project.bri")).await? {
                return self.load(brioche, ancestor, validate).await;
            }
        }

        anyhow::bail!("could not find project root for path {}", path.display());
    }

    pub async fn clear(&self, project_hash: ProjectHash) -> anyhow::Result<bool> {
        let mut projects = self
            .inner
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;

        let project = projects.projects.remove(&project_hash);
        let paths = projects
            .projects_to_paths
            .remove(&project_hash)
            .unwrap_or_default();
        for path in &paths {
            projects.paths_to_projects.remove(path);
        }

        Ok(project.is_some())
    }

    pub fn project_root(&self, project_hash: ProjectHash) -> anyhow::Result<PathBuf> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        let project_root = projects
            .projects_to_paths
            .get(&project_hash)
            .and_then(|paths| paths.iter().next())
            .with_context(|| format!("project root not found for hash {}", project_hash))?;
        Ok(project_root.clone())
    }

    pub fn project_root_module_path(&self, project_hash: ProjectHash) -> anyhow::Result<PathBuf> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.project_root_module_path(project_hash)
    }

    pub fn project_root_module_specifier(
        &self,
        project_hash: ProjectHash,
    ) -> anyhow::Result<super::script::specifier::BriocheModuleSpecifier> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.project_root_module_specifier(project_hash)
    }

    pub fn project_module_paths(&self, project_hash: ProjectHash) -> anyhow::Result<Vec<PathBuf>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        let module_paths = projects.project_module_paths(project_hash)?;
        Ok(module_paths.collect())
    }

    pub fn project_module_specifiers(
        &self,
        project_hash: ProjectHash,
    ) -> anyhow::Result<Vec<super::script::specifier::BriocheModuleSpecifier>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        let module_specifiers = projects.project_module_specifiers(project_hash)?;
        Ok(module_specifiers.collect())
    }

    pub fn find_containing_project(&self, path: &Path) -> anyhow::Result<Option<ProjectHash>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        Ok(projects.find_containing_project(path))
    }

    pub fn find_containing_project_root(
        &self,
        path: &Path,
        project_hash: ProjectHash,
    ) -> anyhow::Result<PathBuf> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        let path = projects.find_containing_project_root(path, project_hash)?;
        Ok(path.to_owned())
    }

    pub fn project(&self, project_hash: ProjectHash) -> anyhow::Result<Arc<Project>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.project(project_hash).cloned()
    }

    pub fn local_paths(&self, project_hash: ProjectHash) -> anyhow::Result<BTreeSet<PathBuf>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        let local_paths = projects
            .local_paths(project_hash)
            .with_context(|| format!("project not found for hash {project_hash}"))?;
        Ok(local_paths.map(|path| path.to_owned()).collect())
    }

    pub fn validate_no_dirty_lockfiles(&self) -> anyhow::Result<()> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        let dirty_lockfile_paths = projects.dirty_lockfiles.keys().cloned().collect::<Vec<_>>();
        anyhow::ensure!(
            dirty_lockfile_paths.is_empty(),
            "dirty lockfiles found: {dirty_lockfile_paths:?}"
        );
        Ok(())
    }

    pub async fn commit_dirty_lockfiles(&self) -> anyhow::Result<usize> {
        let dirty_lockfiles = {
            let projects = self
                .inner
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
            projects.dirty_lockfiles.clone()
        };

        for (path, lockfile) in dirty_lockfiles {
            let mut lockfile_contents = serde_json::to_string_pretty(&lockfile)
                .with_context(|| format!("failed to serialize lockfile at {}", path.display()))?;
            lockfile_contents.push('\n');

            tokio::fs::write(&path, lockfile_contents)
                .await
                .context("failed to write lockfile")?;
        }

        let mut projects = self
            .inner
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        let num_lockfiles = projects.dirty_lockfiles.len();
        projects.dirty_lockfiles.clear();

        Ok(num_lockfiles)
    }

    pub async fn commit_dirty_lockfile_for_project_path(
        &self,
        project_path: &Path,
    ) -> anyhow::Result<bool> {
        let lockfile_path = project_path.join("brioche.lock");

        let dirty_lockfile = {
            let projects = self
                .inner
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
            projects.dirty_lockfiles.get(&lockfile_path).cloned()
        };

        let Some(dirty_lockfile) = dirty_lockfile else {
            return Ok(false);
        };

        let mut lockfile_contents =
            serde_json::to_string_pretty(&dirty_lockfile).with_context(|| {
                format!(
                    "failed to serialize lockfile at {}",
                    lockfile_path.display()
                )
            })?;
        lockfile_contents.push('\n');

        tokio::fs::write(&lockfile_path, lockfile_contents)
            .await
            .context("failed to write lockfile")?;

        let mut projects = self
            .inner
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.dirty_lockfiles.remove(&lockfile_path);

        Ok(true)
    }

    pub fn get_static(
        &self,
        specifier: &super::script::specifier::BriocheModuleSpecifier,
        static_: &StaticQuery,
    ) -> anyhow::Result<Option<RecipeHash>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.get_static(specifier, static_)
    }
}

#[derive(Default, Clone)]
struct ProjectsInner {
    projects: HashMap<ProjectHash, Arc<Project>>,
    paths_to_projects: HashMap<PathBuf, ProjectHash>,
    projects_to_paths: HashMap<ProjectHash, BTreeSet<PathBuf>>,
    dirty_lockfiles: HashMap<PathBuf, Lockfile>,
    project_load_errors: HashMap<ProjectHash, Vec<LoadProjectError>>,
}

impl ProjectsInner {
    fn project(&self, project_hash: ProjectHash) -> anyhow::Result<&Arc<Project>> {
        self.projects
            .get(&project_hash)
            .with_context(|| format!("project not found for hash {project_hash}"))
    }

    fn local_paths(&self, project_hash: ProjectHash) -> Option<impl Iterator<Item = &Path> + '_> {
        let paths = self.projects_to_paths.get(&project_hash)?;
        Some(paths.iter().map(|path| &**path))
    }

    fn find_containing_project(&self, path: &Path) -> Option<ProjectHash> {
        // TODO: Keep a map directly between submodules and project roots

        path.ancestors()
            .find_map(|path| self.paths_to_projects.get(path).copied())
    }

    fn find_containing_project_root(
        &self,
        path: &Path,
        project_hash: ProjectHash,
    ) -> anyhow::Result<&Path> {
        let project_root = self
            .projects_to_paths
            .get(&project_hash)
            .and_then(|paths| {
                paths
                    .iter()
                    .find(|project_root| path.starts_with(project_root))
            })
            .with_context(|| {
                format!(
                    "matching project root not found for path {}",
                    path.display()
                )
            })?;
        Ok(project_root)
    }

    fn project_root_module_path(&self, project_hash: ProjectHash) -> anyhow::Result<PathBuf> {
        let project = self
            .projects
            .get(&project_hash)
            .with_context(|| format!("project not found for hash {project_hash}"))?;
        let project_root = self
            .projects_to_paths
            .get(&project_hash)
            .and_then(|paths| paths.first())
            .with_context(|| format!("project root not found for hash {project_hash}"))?;

        let root_relative_path = RelativePath::new("project.bri");
        anyhow::ensure!(
            project.modules.contains_key(root_relative_path),
            "root module not found for project {}",
            project_root.display()
        );

        let root_path = root_relative_path.to_logical_path(project_root);
        assert!(
            root_path.starts_with(project_root),
            "module path {} escapes project root {}",
            root_path.display(),
            project_root.display()
        );
        Ok(root_path)
    }

    pub fn project_root_module_specifier(
        &self,
        project_hash: ProjectHash,
    ) -> anyhow::Result<super::script::specifier::BriocheModuleSpecifier> {
        let path = self.project_root_module_path(project_hash)?;
        Ok(super::script::specifier::BriocheModuleSpecifier::File { path })
    }

    pub fn project_module_paths(
        &self,
        project_hash: ProjectHash,
    ) -> anyhow::Result<impl Iterator<Item = PathBuf> + '_> {
        let project = self
            .projects
            .get(&project_hash)
            .with_context(|| format!("project not found for hash {project_hash}"))?;
        let project_root = self
            .projects_to_paths
            .get(&project_hash)
            .and_then(|paths| paths.first())
            .with_context(|| format!("project root not found for hash {project_hash}"))?;

        let paths = project.modules.keys().map(move |module_path| {
            let path = module_path.to_logical_path(project_root);
            assert!(
                path.starts_with(project_root),
                "module path {module_path} escapes project root {}",
                project_root.display()
            );
            path
        });
        Ok(paths)
    }

    pub fn project_module_specifiers(
        &self,
        project_hash: ProjectHash,
    ) -> anyhow::Result<impl Iterator<Item = super::script::specifier::BriocheModuleSpecifier> + '_>
    {
        let module_paths = self.project_module_paths(project_hash)?;
        let module_specifiers = module_paths
            .map(|path| super::script::specifier::BriocheModuleSpecifier::File { path });
        Ok(module_specifiers)
    }

    pub fn get_static(
        &self,
        specifier: &super::script::specifier::BriocheModuleSpecifier,
        static_: &StaticQuery,
    ) -> anyhow::Result<Option<RecipeHash>> {
        let path = match specifier {
            super::script::specifier::BriocheModuleSpecifier::File { path } => path,
            _ => {
                anyhow::bail!("could not get static for specifier {specifier}");
            }
        };

        let project_hash = self
            .find_containing_project(path)
            .with_context(|| format!("project not found for specifier {specifier}"))?;
        let project = self
            .projects
            .get(&project_hash)
            .with_context(|| format!("project not found for hash {project_hash}"))?;
        let project_root = self
            .projects_to_paths
            .get(&project_hash)
            .and_then(|paths| paths.first())
            .with_context(|| format!("project root not found for hash {project_hash}"))?;

        let module_path = path.relative_to(project_root).with_context(|| {
            format!(
                "failed to get relative path from {} to {}",
                path.display(),
                project_root.display()
            )
        })?;

        let Some(statics) = project.statics.get(&module_path) else {
            return Ok(None);
        };
        let Some(Some(static_)) = statics.get(static_) else {
            return Ok(None);
        };
        Ok(Some(*static_))
    }
}

async fn load_project(
    projects: Projects,
    brioche: Brioche,
    path: PathBuf,
    fully_valid: bool,
    depth: usize,
) -> anyhow::Result<ProjectHash> {
    let rt = tokio::runtime::Handle::current();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let local_set = tokio::task::LocalSet::new();

        local_set.spawn_local(async move {
            let result =
                load_project_inner(&projects, &brioche, &path, fully_valid, false, depth).await;
            let _ = tx.send(result).inspect_err(|err| {
                tracing::warn!("failed to send project load result: {err:?}");
            });
        });

        rt.block_on(local_set);
    });

    let (project_hash, _, _) = rx.await.context("failed to get project load result")??;
    Ok(project_hash)
}

#[async_recursion::async_recursion(?Send)]
async fn load_project_inner(
    projects: &Projects,
    brioche: &Brioche,
    path: &Path,
    fully_valid: bool,
    lockfile_required: bool,
    depth: usize,
) -> anyhow::Result<(ProjectHash, Arc<Project>, Vec<LoadProjectError>)> {
    tracing::debug!(path = %path.display(), "resolving project");

    let path = tokio::fs::canonicalize(path)
        .await
        .with_context(|| format!("failed to canonicalize path {}", path.display()))?;
    let workspace = find_workspace(&path).await?;

    let project_analysis = analyze::analyze_project(&brioche.vfs, &path).await?;

    let lockfile_path = path.join("brioche.lock");
    let lockfile_contents = tokio::fs::read_to_string(&lockfile_path).await;
    let lockfile: Option<Lockfile> = match lockfile_contents {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(lockfile) => Some(lockfile),
            Err(error) => {
                if lockfile_required {
                    return Err(error).context(format!(
                        "failed to parse lockfile at {}",
                        lockfile_path.display()
                    ));
                } else {
                    None
                }
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if lockfile_required {
                anyhow::bail!("lockfile not found: {}", lockfile_path.display());
            } else {
                None
            }
        }
        Err(error) => {
            return Err(error).context(format!(
                "failed to read lockfile at {}",
                lockfile_path.display()
            ));
        }
    };

    let mut new_lockfile = Lockfile::default();
    let mut errors = vec![];

    static DEPENDENCY_NAME_REGEX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let dependency_name_regex = DEPENDENCY_NAME_REGEX
        .get_or_init(|| regex::Regex::new("^[a-zA-Z0-9_]+$").expect("failed to compile regex"));

    let dep_depth = depth
        .checked_sub(1)
        .context("project dependency depth exceeded")?;
    let mut dependencies = HashMap::new();
    for (name, dependency_def) in &project_analysis.definition.dependencies {
        anyhow::ensure!(
            dependency_name_regex.is_match(name),
            "invalid dependency name"
        );

        let dependency_hash = match dependency_def {
            DependencyDefinition::Path { path: subpath } => {
                let dep_path = path.join(subpath);
                let load_result = try_load_path_dependency_with_errors(
                    projects,
                    brioche,
                    name,
                    &dep_path,
                    fully_valid,
                    lockfile_required,
                    dep_depth,
                    &mut errors,
                )
                .await;
                let Some(dep_hash) = load_result else {
                    continue;
                };

                dep_hash
            }
            DependencyDefinition::Version(version) => {
                let load_result = try_load_registry_dependency_with_errors(
                    projects,
                    brioche,
                    workspace.as_ref(),
                    name,
                    version,
                    fully_valid,
                    lockfile_required,
                    lockfile.as_ref(),
                    dep_depth,
                    &mut new_lockfile,
                    &mut errors,
                )
                .await;
                let Some(dep_hash) = load_result else {
                    continue;
                };

                dep_hash
            }
        };

        dependencies.insert(name.to_owned(), dependency_hash);
    }

    for module in project_analysis.local_modules.values() {
        for import_analysis in module.imports.values() {
            let analyze::ImportAnalysis::ExternalProject(dep_name) = import_analysis else {
                continue;
            };

            anyhow::ensure!(
                dependency_name_regex.is_match(dep_name),
                "invalid imported dependency name: {dep_name}",
            );

            match dependencies.entry(dep_name.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    // Dependency already exists
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    // Dependency not included explicitly, so load it from
                    // the registry. Equivalent to using a version of `*`
                    let load_result = try_load_registry_dependency_with_errors(
                        projects,
                        brioche,
                        workspace.as_ref(),
                        dep_name,
                        &Version::Any,
                        fully_valid,
                        lockfile_required,
                        lockfile.as_ref(),
                        dep_depth,
                        &mut new_lockfile,
                        &mut errors,
                    )
                    .await;
                    let Some(dep_hash) = load_result else {
                        continue;
                    };

                    // Add it as a dependency
                    entry.insert(dep_hash);
                }
            }
        }
    }

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
            if fully_valid {
                let recipe_hash = resolve_static(brioche, &path, module, static_).await?;
                module_statics.insert(static_.clone(), Some(recipe_hash));
            } else {
                module_statics.insert(static_.clone(), None);
            }
        }

        if !module_statics.is_empty() {
            statics.insert(module.project_subpath.clone(), module_statics);
        }
    }

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
        let mut projects = projects
            .inner
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;

        if lockfile.as_ref() != Some(&new_lockfile) {
            if lockfile_required {
                anyhow::bail!("lockfile at {} is out of date", lockfile_path.display());
            } else {
                projects.dirty_lockfiles.insert(lockfile_path, new_lockfile);
            }
        }

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

#[allow(clippy::too_many_arguments)]
async fn try_load_path_dependency_with_errors(
    projects: &Projects,
    brioche: &Brioche,
    name: &str,
    dep_path: &Path,
    fully_valid: bool,
    lockfile_required: bool,
    dep_depth: usize,
    errors: &mut Vec<LoadProjectError>,
) -> Option<ProjectHash> {
    let result = load_project_inner(
        projects,
        brioche,
        dep_path,
        fully_valid,
        lockfile_required,
        dep_depth,
    )
    .await;

    match result {
        Ok((dep_hash, _, dep_errors)) => {
            errors.extend(
                dep_errors
                    .into_iter()
                    .map(|error| LoadProjectError::DependencyError {
                        name: name.to_owned(),
                        error: Box::new(error),
                    }),
            );

            Some(dep_hash)
        }
        Err(err) => {
            errors.push(LoadProjectError::FailedToLoadDependency {
                name: name.to_owned(),
                cause: err.to_string(),
            });
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn try_load_registry_dependency_with_errors(
    projects: &Projects,
    brioche: &Brioche,
    workspace: Option<&Workspace>,
    name: &str,
    version: &Version,
    fully_valid: bool,
    lockfile_required: bool,
    lockfile: Option<&Lockfile>,
    dep_depth: usize,
    new_lockfile: &mut Lockfile,
    errors: &mut Vec<LoadProjectError>,
) -> Option<ProjectHash> {
    let resolved_dep_result = resolve_dependency_to_local_path(
        brioche,
        workspace,
        name,
        version,
        lockfile_required,
        lockfile,
    )
    .await;
    let resolved_dep = match resolved_dep_result {
        Ok(resolved_dep) => resolved_dep,
        Err(err) => {
            errors.push(LoadProjectError::FailedToLoadDependency {
                name: name.to_owned(),
                cause: err.to_string(),
            });
            return None;
        }
    };

    let result = load_project_inner(
        projects,
        brioche,
        &resolved_dep.local_path,
        fully_valid,
        resolved_dep.lockfile_required,
        dep_depth,
    )
    .await;
    let (actual_hash, _, dep_errors) = match result {
        Ok(dep) => dep,
        Err(err) => {
            errors.push(LoadProjectError::FailedToLoadDependency {
                name: name.to_owned(),
                cause: err.to_string(),
            });
            return None;
        }
    };

    if let Some(expected_hash) = resolved_dep.expected_hash {
        if expected_hash != actual_hash {
            errors.push(LoadProjectError::FailedToLoadDependency {
                name: name.to_owned(),
                cause: format!(
                    "resolved dependency at '{}' did not match expected hash",
                    resolved_dep.local_path.display()
                ),
            });
        }
    }

    errors.extend(
        dep_errors
            .into_iter()
            .map(|error| LoadProjectError::DependencyError {
                name: name.to_owned(),
                error: Box::new(error),
            }),
    );

    if let Some(should_lock) = resolved_dep.should_lock {
        new_lockfile
            .dependencies
            .insert(name.to_owned(), should_lock);
    }

    Some(actual_hash)
}

async fn resolve_dependency_to_local_path(
    brioche: &Brioche,
    workspace: Option<&Workspace>,
    dependency_name: &str,
    dependency_version: &Version,
    lockfile_required: bool,
    lockfile: Option<&Lockfile>,
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
                lockfile_required,
                should_lock: None,
            });
        }
    }

    // TODO: Validate that the requested dependency version matches the
    // version in the lockfile
    let lockfile_dep_hash =
        lockfile.and_then(|lockfile| lockfile.dependencies.get(dependency_name));
    let dep_hash = match lockfile_dep_hash {
        Some(dep_hash) => *dep_hash,
        None => {
            if lockfile_required {
                anyhow::bail!("dependency '{}' not found in lockfile", dependency_name);
            } else {
                resolve_project_from_registry(brioche, dependency_name, dependency_version)
                    .await
                    .with_context(|| {
                        format!("failed to resolve '{dependency_name}' from registry")
                    })?
            }
        }
    };

    let local_path = fetch_project_from_registry(brioche, dep_hash)
        .await
        .with_context(|| format!("failed to fetch '{dependency_name}' from registry"))?;

    Ok(ResolvedDependency {
        local_path,
        expected_hash: Some(dep_hash),
        lockfile_required: true,
        should_lock: Some(dep_hash),
    })
}

struct ResolvedDependency {
    local_path: PathBuf,
    expected_hash: Option<ProjectHash>,
    lockfile_required: bool,
    should_lock: Option<ProjectHash>,
}

async fn resolve_project_from_registry(
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

async fn fetch_project_from_registry(
    brioche: &Brioche,
    project_hash: ProjectHash,
) -> anyhow::Result<PathBuf> {
    let local_path = brioche.home.join("projects").join(project_hash.to_string());

    if tokio::fs::try_exists(&local_path).await? {
        return Ok(local_path);
    }

    let temp_id = ulid::Ulid::new();
    let temp_project_path = brioche.home.join("projects-temp").join(temp_id.to_string());
    tokio::fs::create_dir_all(&temp_project_path).await?;

    let project = brioche
        .registry_client
        .get_project(project_hash)
        .await
        .context("failed to get project metadata from registry")?;

    for dep_hash in project.dependency_hashes() {
        Box::pin(fetch_project_from_registry(brioche, dep_hash)).await?;
    }

    let statics_recipes = project
        .statics
        .values()
        .flat_map(|module_statics| module_statics.values().filter_map(|recipe| *recipe))
        .collect::<HashSet<_>>();
    crate::registry::fetch_recipes(brioche, &statics_recipes).await?;

    for (module_path, statics) in &project.statics {
        for (static_, recipe_hash) in statics {
            let Some(recipe_hash) = recipe_hash else {
                continue;
            };

            let module_path = module_path.to_logical_path(&temp_project_path);
            let module_dir = module_path.parent().context("no parent dir for module")?;

            match static_ {
                StaticQuery::Include(include) => {
                    let recipe = crate::recipe::get_recipe(brioche, *recipe_hash).await?;
                    let artifact: crate::recipe::Artifact = recipe.try_into().map_err(|_| {
                        anyhow::anyhow!("included static recipe is not an artifact")
                    })?;
                    let include_path = module_dir.join(include.path());
                    crate::output::create_output(
                        brioche,
                        &artifact,
                        crate::output::OutputOptions {
                            link_locals: false,
                            merge: true,
                            mtime: None,
                            output_path: &include_path,
                            resources_dir: None,
                        },
                    )
                    .await?;
                }
                StaticQuery::Glob { .. } => {
                    let recipe = crate::recipe::get_recipe(brioche, *recipe_hash).await?;
                    let artifact: crate::recipe::Artifact = recipe.try_into().map_err(|_| {
                        anyhow::anyhow!("included static recipe is not an artifact")
                    })?;
                    crate::output::create_output(
                        brioche,
                        &artifact,
                        crate::output::OutputOptions {
                            link_locals: false,
                            merge: true,
                            mtime: None,
                            output_path: module_dir,
                            resources_dir: None,
                        },
                    )
                    .await?;
                }
            }
        }
    }

    for (module_path, file_id) in &project.modules {
        anyhow::ensure!(
            module_path != "brioche.lock",
            "lockfile included as a project module"
        );

        let temp_module_path = module_path.to_logical_path(&temp_project_path);
        anyhow::ensure!(
            temp_module_path.starts_with(&temp_project_path),
            "module path escapes project root",
        );

        let blob_hash = file_id.as_blob_hash()?;
        let module_content = brioche
            .registry_client
            .get_blob(blob_hash)
            .await
            .context("failed to get blob from registry")?;
        if let Some(temp_module_dir) = temp_module_path.parent() {
            tokio::fs::create_dir_all(temp_module_dir)
                .await
                .context("failed to create temporary module directory")?;
        }
        tokio::fs::write(&temp_module_path, &module_content)
            .await
            .context("failed to write blob")?;
    }

    let lockfile = Lockfile {
        dependencies: project
            .dependencies
            .iter()
            .map(|(name, hash)| (name.clone(), *hash))
            .collect(),
    };
    let lockfile_path = temp_project_path.join("brioche.lock");
    let lockfile_contents =
        serde_json::to_string_pretty(&lockfile).context("failed to serialize lockfile")?;
    let mut lockfile_file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lockfile_path)
        .await
        .context("failed to create lockfile")?;
    lockfile_file
        .write_all(lockfile_contents.as_bytes())
        .await
        .context("failed to write lockfile")?;

    if let Some(local_dir) = local_path.parent() {
        tokio::fs::create_dir_all(local_dir)
            .await
            .context("failed to create project directory")?;
    }

    tokio::fs::rename(&temp_project_path, &local_path)
        .await
        .context("failed to move temporary project from registry")?;
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
    module: &analyze::ModuleAnalysis,
    static_: &analyze::StaticQuery,
) -> anyhow::Result<RecipeHash> {
    match static_ {
        analyze::StaticQuery::Include(include) => {
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
                    meta: &Default::default(),
                    remove_input: false,
                    resources_dir: None,
                },
            )
            .await?;
            match (&include, &artifact.value) {
                (analyze::StaticInclude::File { .. }, Artifact::File(_))
                | (analyze::StaticInclude::Directory { .. }, Artifact::Directory(_)) => {
                    // Valid
                }
                (analyze::StaticInclude::File { path }, _) => {
                    anyhow::bail!("expected path {path:?} to be a file");
                }
                (analyze::StaticInclude::Directory { path }, _) => {
                    anyhow::bail!("expected path {path:?} to be a directory");
                }
            }
            let artifact_hash = artifact.hash();

            let recipe = crate::recipe::Recipe::from(artifact.value);
            crate::recipe::save_recipes(brioche, [&recipe]).await?;

            Ok(artifact_hash)
        }
        analyze::StaticQuery::Glob { patterns } => {
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
                        let relative_entry_path =
                            pathdiff::diff_paths(entry.path(), &module_dir_path).with_context(
                                || {
                                    format!(
                                    "failed to resolve matched path {} relative to module path {}",
                                    entry.path().display(),
                                    module_dir_path.display(),
                                )
                                },
                            )?;
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
                            meta: &Default::default(),
                            remove_input: false,
                            resources_dir: None,
                        },
                    )
                    .await?;
                    anyhow::Ok((relative_path, artifact))
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

            Ok(recipe_hash)
        }
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub definition: ProjectDefinition,
    pub dependencies: HashMap<String, ProjectHash>,
    pub modules: HashMap<RelativePathBuf, FileId>,
    #[serde_as(as = "HashMap<_, Vec<(_, _)>>")]
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub statics: HashMap<RelativePathBuf, BTreeMap<StaticQuery, Option<RecipeHash>>>,
}

impl Project {
    pub fn dependencies(&self) -> impl Iterator<Item = (&str, ProjectHash)> {
        self.dependencies
            .iter()
            .map(|(name, hash)| (name.as_str(), *hash))
    }

    pub fn dependency_hashes(&self) -> impl Iterator<Item = ProjectHash> + '_ {
        self.dependencies().map(|(_, hash)| hash)
    }

    pub fn dependency_hash(&self, name: &str) -> Option<ProjectHash> {
        self.dependencies.get(name).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoadProjectError {
    FailedToLoadDependency {
        name: String,
        cause: String,
    },
    DependencyError {
        name: String,
        error: Box<LoadProjectError>,
    },
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
    Path { path: PathBuf },
    Version(Version),
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
            _ => anyhow::bail!("unsupported version specifier: {}", s),
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
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde_with::SerializeDisplay,
    serde_with::DeserializeFromStr,
)]
pub struct ProjectHash(blake3::Hash);

impl ProjectHash {
    fn from_serializable<V>(value: &V) -> anyhow::Result<Self>
    where
        V: serde::Serialize,
    {
        let mut hasher = blake3::Hasher::new();

        json_canon::to_writer(&mut hasher, value)?;

        let hash = hasher.finalize();
        Ok(Self(hash))
    }

    pub fn validate_matches(&self, project: &Project) -> anyhow::Result<()> {
        let actual_hash = ProjectHash::from_serializable(project)?;
        anyhow::ensure!(
            self == &actual_hash,
            "project hash does not match expected hash"
        );
        Ok(())
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(blake3::Hash::from_bytes(bytes))
    }

    pub fn try_from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let bytes = bytes.try_into()?;
        Ok(Self::from_bytes(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    pub fn as_slice(&self) -> &[u8] {
        self.as_bytes().as_slice()
    }

    pub fn blake3(&self) -> &blake3::Hash {
        &self.0
    }
}

impl std::fmt::Display for ProjectHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ProjectHash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hash = blake3::Hash::from_hex(s)?;
        Ok(Self(hash))
    }
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub definition: WorkspaceDefinition,
    pub path: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceDefinition {
    pub members: Vec<WorkspaceMember>,
}

#[derive(Debug, Clone, serde_with::SerializeDisplay, serde_with::DeserializeFromStr)]
pub enum WorkspaceMember {
    Path(RelativePathBuf, String),
    WildcardPath(RelativePathBuf),
}

impl std::str::FromStr for WorkspaceMember {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path = RelativePath::new(s);
        let Some(last_component) = path.components().next_back() else {
            anyhow::bail!("invalid workspace member path: {s}");
        };

        let last_component = match last_component {
            relative_path::Component::CurDir | relative_path::Component::ParentDir => {
                anyhow::bail!("invalid workspace member path: {s}");
            }
            relative_path::Component::Normal(component) => component,
        };

        // Shouldn't fail since we already validated the last component exists
        let path = path.parent().expect("parent path not found").to_owned();

        for component in path.components() {
            match component {
                relative_path::Component::ParentDir => {
                    anyhow::bail!("invalid workspace member path: {s}");
                }
                relative_path::Component::CurDir => {}
                relative_path::Component::Normal(component) => {
                    anyhow::ensure!(
                        !component.contains('*'),
                        "invalid wildcard in workspace member path: {s}"
                    );
                }
            }
        }

        match last_component {
            "*" => Ok(Self::WildcardPath(path)),
            invalid if invalid.contains('*') => {
                anyhow::bail!("invalid wildcard in workspace member path: {s}");
            }
            name => Ok(Self::Path(path, name.to_string())),
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

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lockfile {
    pub dependencies: BTreeMap<String, ProjectHash>,
}
