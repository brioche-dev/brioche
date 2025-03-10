use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use analyze::{GitRefOptions, StaticOutput, StaticOutputKind, StaticQuery};
use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use relative_path::{PathExt as _, RelativePath, RelativePathBuf};
use tokio::io::AsyncReadExt as _;

use crate::recipe::Artifact;

use super::{Brioche, vfs::FileId};

pub mod analyze;
pub mod artifact;

#[derive(Debug, Clone, Copy)]
pub enum ProjectValidation {
    /// Fully validate the project, ensuring all modules and dependencies
    /// resolve properly
    Standard,

    /// Perform minimal validation while loading the project. This is suitable
    /// for situations like the LSP, where syntax errors are common
    Minimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectLocking {
    /// The project is unlocked, meaning the lockfile can be updated
    Unlocked,

    /// The project is locked, meaning the lockfile must exist and be
    /// up-to-date. This is useful for loading registry projects, or when
    /// the `--locked` CLI flag is specified
    Locked,
}

#[derive(Clone, Default)]
pub struct Projects {
    inner: Arc<std::sync::RwLock<ProjectsInner>>,
}

impl Projects {
    pub async fn load(
        &self,
        brioche: &Brioche,
        path: &Path,
        validation: ProjectValidation,
        locking: ProjectLocking,
    ) -> anyhow::Result<ProjectHash> {
        {
            let projects = self
                .inner
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
            if let Some(project_hash) = projects.paths_to_projects.get(path) {
                match validation {
                    ProjectValidation::Standard => {
                        let errors = &projects.project_load_errors[project_hash];
                        if !errors.is_empty() {
                            anyhow::bail!("project load errors: {errors:?}");
                        }
                    }
                    ProjectValidation::Minimal => {}
                }

                return Ok(*project_hash);
            }
        }

        let project_hash = load_project(
            self.clone(),
            brioche.clone(),
            path.to_owned(),
            validation,
            locking,
            100,
        )
        .await?;

        match validation {
            ProjectValidation::Standard => {
                let projects = self
                    .inner
                    .read()
                    .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
                let errors = &projects.project_load_errors[&project_hash];
                if !errors.is_empty() {
                    anyhow::bail!("project load errors: {errors:?}");
                }
            }
            ProjectValidation::Minimal => {}
        }

        Ok(project_hash)
    }

    pub async fn load_from_module_path(
        &self,
        brioche: &Brioche,
        path: &Path,
        validation: ProjectValidation,
        locking: ProjectLocking,
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
                return self.load(brioche, ancestor, validation, locking).await;
            }
        }

        anyhow::bail!("could not find project root for path {}", path.display());
    }

    pub async fn load_from_registry(
        &self,
        brioche: &Brioche,
        project_name: &str,
        version: &Version,
    ) -> anyhow::Result<ProjectHash> {
        let project_hash = resolve_project_from_registry(brioche, project_name, version)
            .await
            .with_context(|| format!("failed to resolve '{project_name}' from registry"))?;
        let local_path =
            fetch_project_from_cache(self, brioche, project_hash, ProjectValidation::Standard)
                .await
                .with_context(|| format!("failed to fetch '{project_name}' from registry"))?;

        let loaded_project_hash = self
            .load(
                brioche,
                &local_path,
                ProjectValidation::Standard,
                ProjectLocking::Locked,
            )
            .await?;

        anyhow::ensure!(
            loaded_project_hash == project_hash,
            "expected {project_name} {version} to have hash {project_hash}, but got {loaded_project_hash} (loaded from {})",
            local_path.display(),
        );

        Ok(loaded_project_hash)
    }

    pub fn clear(&self, project_hash: ProjectHash) -> anyhow::Result<bool> {
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
            .with_context(|| format!("project root not found for hash {project_hash}"))?;
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
    ) -> anyhow::Result<Option<StaticOutput>> {
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
    ) -> anyhow::Result<Option<StaticOutput>> {
        let super::script::specifier::BriocheModuleSpecifier::File { path } = specifier else {
            anyhow::bail!("could not get static for specifier {specifier}");
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
        let Some(Some(output)) = statics.get(static_) else {
            return Ok(None);
        };
        Ok(Some(output.clone()))
    }
}

async fn load_project(
    projects: Projects,
    brioche: Brioche,
    path: PathBuf,
    validation: ProjectValidation,
    locking: ProjectLocking,
    depth: usize,
) -> anyhow::Result<ProjectHash> {
    let rt = tokio::runtime::Handle::current();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let local_set = tokio::task::LocalSet::new();

        local_set.spawn_local(async move {
            let result =
                load_project_inner(&projects, &brioche, &path, validation, locking, depth).await;
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
    validation: ProjectValidation,
    locking: ProjectLocking,
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
            Err(error) => match locking {
                ProjectLocking::Locked => {
                    return Err(error).context(format!(
                        "failed to parse lockfile at {}",
                        lockfile_path.display()
                    ));
                }
                ProjectLocking::Unlocked => None,
            },
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => match locking {
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
                    validation,
                    locking,
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
                    validation,
                    locking,
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
                        validation,
                        locking,
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
            match validation {
                ProjectValidation::Standard => {
                    let recipe_hash = resolve_static(
                        brioche,
                        &path,
                        module,
                        static_,
                        locking,
                        lockfile.as_ref(),
                        &mut new_lockfile,
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

        // If the lockfile doesn't need to be fully valid, ensure that
        // the new lockfile includes old statics and dependencies that
        // weren't updated. This can mean that e.g. unnecessary downloads
        // and dependencies are kept, but this is appropriate for
        // situations like the LSP
        match validation {
            ProjectValidation::Standard => {}
            ProjectValidation::Minimal => {
                let Lockfile {
                    dependencies: new_dependencies,
                    downloads: new_downloads,
                    git_refs: new_git_refs,
                } = &mut new_lockfile;

                if let Some(lockfile) = &lockfile {
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

        if lockfile.as_ref() != Some(&new_lockfile) {
            match locking {
                ProjectLocking::Unlocked => {
                    projects.dirty_lockfiles.insert(lockfile_path, new_lockfile);
                }
                ProjectLocking::Locked => {
                    anyhow::bail!("lockfile at {} is out of date", lockfile_path.display());
                }
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

#[expect(clippy::too_many_arguments)]
async fn try_load_path_dependency_with_errors(
    projects: &Projects,
    brioche: &Brioche,
    name: &str,
    dep_path: &Path,
    validation: ProjectValidation,
    locking: ProjectLocking,
    dep_depth: usize,
    errors: &mut Vec<LoadProjectError>,
) -> Option<ProjectHash> {
    let result =
        load_project_inner(projects, brioche, dep_path, validation, locking, dep_depth).await;

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
        Err(error) => {
            errors.push(LoadProjectError::FailedToLoadDependency {
                name: name.to_owned(),
                cause: format!("{error:#}"),
            });
            None
        }
    }
}

#[expect(clippy::too_many_arguments)]
async fn try_load_registry_dependency_with_errors(
    projects: &Projects,
    brioche: &Brioche,
    workspace: Option<&Workspace>,
    name: &str,
    version: &Version,
    validation: ProjectValidation,
    locking: ProjectLocking,
    lockfile: Option<&Lockfile>,
    dep_depth: usize,
    new_lockfile: &mut Lockfile,
    errors: &mut Vec<LoadProjectError>,
) -> Option<ProjectHash> {
    let resolved_dep_result = resolve_dependency_to_local_path(
        projects, brioche, workspace, name, version, validation, locking, lockfile,
    )
    .await;
    let resolved_dep = match resolved_dep_result {
        Ok(resolved_dep) => resolved_dep,
        Err(error) => {
            errors.push(LoadProjectError::FailedToLoadDependency {
                name: name.to_owned(),
                cause: format!("{error:#}"),
            });
            return None;
        }
    };

    let result = load_project_inner(
        projects,
        brioche,
        &resolved_dep.local_path,
        validation,
        resolved_dep.locking,
        dep_depth,
    )
    .await;
    let (actual_hash, _, dep_errors) = match result {
        Ok(dep) => dep,
        Err(error) => {
            errors.push(LoadProjectError::FailedToLoadDependency {
                name: name.to_owned(),
                cause: format!("{error:#}"),
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

#[expect(clippy::too_many_arguments)]
async fn resolve_dependency_to_local_path(
    projects: &Projects,
    brioche: &Brioche,
    workspace: Option<&Workspace>,
    dependency_name: &str,
    dependency_version: &Version,
    validation: ProjectValidation,
    locking: ProjectLocking,
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
                locking,
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
        None => match locking {
            ProjectLocking::Unlocked => {
                resolve_project_from_registry(brioche, dependency_name, dependency_version)
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

    let local_path = fetch_project_from_cache(projects, brioche, dep_hash, validation)
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

async fn fetch_project_from_cache(
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
        artifact::save_projects_from_artifact(brioche, projects, &project_artifact).await?;
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
    module: &analyze::ModuleAnalysis,
    static_: &analyze::StaticQuery,
    locking: ProjectLocking,
    lockfile: Option<&Lockfile>,
    new_lockfile: &mut Lockfile,
) -> anyhow::Result<StaticOutput> {
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
                    remove_input: false,
                    resource_dir: None,
                    input_resource_dirs: &[],
                    saved_paths: &mut Default::default(),
                    meta: &Default::default(),
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

            Ok(StaticOutput::RecipeHash(artifact_hash))
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
            let current_download_hash = lockfile.and_then(|lockfile| lockfile.downloads.get(url));

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
            new_lockfile
                .downloads
                .insert(url.clone(), download_hash.clone());

            Ok(StaticOutput::Kind(StaticOutputKind::Download {
                hash: download_hash,
            }))
        }
        StaticQuery::GitRef(GitRefOptions { repository, ref_ }) => {
            let current_commit = lockfile.and_then(|lockfile| {
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
            let repo_refs = new_lockfile.git_refs.entry(repository.clone()).or_default();
            repo_refs.insert(ref_.clone(), commit.clone());

            Ok(StaticOutput::Kind(StaticOutputKind::GitRef { commit }))
        }
    }
}

// TODO: This should be refactored to share code with `load_project_inner`
async fn local_project_hash(brioche: &Brioche, path: &Path) -> anyhow::Result<Option<ProjectHash>> {
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

    let project_analysis = analyze::analyze_project(&brioche.vfs, &path).await;
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
                Some(&lockfile),
                &mut Lockfile::default(),
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

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub definition: ProjectDefinition,
    pub dependencies: HashMap<String, ProjectHash>,
    pub modules: HashMap<RelativePathBuf, FileId>,
    #[serde_as(as = "HashMap<_, Vec<(_, _)>>")]
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub statics: HashMap<RelativePathBuf, BTreeMap<StaticQuery, Option<StaticOutput>>>,
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
        let actual_hash = Self::from_serializable(project)?;
        anyhow::ensure!(
            self == &actual_hash,
            "project hash does not match expected hash"
        );
        Ok(())
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(blake3::Hash::from_bytes(bytes))
    }

    pub fn try_from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let bytes = bytes.try_into()?;
        Ok(Self::from_bytes(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    pub const fn as_slice(&self) -> &[u8] {
        self.as_bytes().as_slice()
    }

    pub const fn blake3(&self) -> &blake3::Hash {
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

impl std::cmp::Ord for ProjectHash {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_bytes().cmp(other.0.as_bytes())
    }
}

impl std::cmp::PartialOrd for ProjectHash {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
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

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub downloads: BTreeMap<url::Url, crate::Hash>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub git_refs: BTreeMap<url::Url, BTreeMap<String, String>>,
}

fn project_lockfile(project: &Project) -> Lockfile {
    let dependencies = project
        .dependencies
        .iter()
        .map(|(name, hash)| (name.clone(), *hash))
        .collect();

    let mut downloads = BTreeMap::new();
    let mut git_refs = BTreeMap::new();
    for (static_, output) in project.statics.values().flatten() {
        match static_ {
            StaticQuery::Include(_) | StaticQuery::Glob { .. } => {
                continue;
            }
            StaticQuery::Download { url } => {
                let Some(StaticOutput::Kind(StaticOutputKind::Download { hash })) = output else {
                    continue;
                };

                downloads.insert(url.clone(), hash.clone());
            }
            StaticQuery::GitRef(GitRefOptions { repository, ref_ }) => {
                let Some(StaticOutput::Kind(StaticOutputKind::GitRef { commit })) = output else {
                    continue;
                };

                let repo_refs: &mut BTreeMap<_, _> =
                    git_refs.entry(repository.clone()).or_default();
                repo_refs.insert(ref_.clone(), commit.clone());
            }
        }
    }

    Lockfile {
        dependencies,
        downloads,
        git_refs,
    }
}
