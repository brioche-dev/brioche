use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use analyze::{GitRefOptions, StaticOutput, StaticOutputKind, StaticQuery};
use anyhow::Context as _;
use relative_path::{PathExt as _, RelativePath, RelativePathBuf};

use super::{Brioche, vfs::FileId};

pub mod analyze;
pub mod artifact;
mod load;

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

        let project_hash = load::load_project(
            self.clone(),
            brioche.clone(),
            path.to_owned(),
            validation,
            locking,
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
        let project_hash = load::resolve_project_from_registry(brioche, project_name, version)
            .await
            .with_context(|| format!("failed to resolve '{project_name}' from registry"))?;
        let local_path = load::fetch_project_from_cache(self, brioche, project_hash)
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

    pub fn project_entry(&self, project_hash: ProjectHash) -> anyhow::Result<ProjectEntry> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.project_entry(project_hash).cloned()
    }

    pub fn project(&self, project_hash: ProjectHash) -> anyhow::Result<Arc<Project>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.project(project_hash).cloned()
    }

    pub fn project_dependencies(
        &self,
        project_hash: ProjectHash,
    ) -> anyhow::Result<HashMap<String, ProjectHash>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.project_dependencies(project_hash)
    }

    fn workspace(&self, workspace_hash: WorkspaceHash) -> anyhow::Result<Arc<Workspace>> {
        let projects = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire 'projects' lock"))?;
        projects.workspace(workspace_hash).cloned()
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
    projects: HashMap<ProjectHash, ProjectEntry>,
    workspaces: HashMap<WorkspaceHash, Arc<Workspace>>,
    paths_to_projects: HashMap<PathBuf, ProjectHash>,
    projects_to_paths: HashMap<ProjectHash, BTreeSet<PathBuf>>,
    dirty_lockfiles: HashMap<PathBuf, Lockfile>,
    project_load_errors: HashMap<ProjectHash, Vec<load::LoadProjectError>>,
}

impl ProjectsInner {
    fn project_entry(&self, project_hash: ProjectHash) -> anyhow::Result<&ProjectEntry> {
        self.projects
            .get(&project_hash)
            .with_context(|| format!("project not found for hash {project_hash}"))
    }

    fn project(&self, project_hash: ProjectHash) -> anyhow::Result<&Arc<Project>> {
        let project_entry = self.project_entry(project_hash)?;
        match project_entry {
            ProjectEntry::WorkspaceMember { workspace, path } => {
                let project = &self.workspaces[workspace].members[path];
                Ok(project)
            }
            ProjectEntry::Project(project) => Ok(project),
        }
    }

    fn project_dependencies(
        &self,
        project_hash: ProjectHash,
    ) -> anyhow::Result<HashMap<String, ProjectHash>> {
        let project_entry = self.project_entry(project_hash)?;
        let dependencies = match project_entry {
            ProjectEntry::WorkspaceMember { workspace: workspace_hash, path } => {
                let workspace = &self.workspaces[workspace_hash];
                let project = &workspace.members[path];
                project.dependencies.iter().map(|(dep_name, dep_ref)| {
                    let dep_hash = match dep_ref {
                        DependencyRef::WorkspaceMember { path: dep_path } => {
                            ProjectHash::from_serializable(&ProjectEntry::WorkspaceMember { workspace: *workspace_hash, path: dep_path.clone() }).expect("failed to calculate ProjectHash")
                        },
                        DependencyRef::Project(project_hash) => {
                            *project_hash
                        },
                    };
                    (dep_name.clone(), dep_hash)
                }).collect()
            }
            ProjectEntry::Project(project) => {
                project.dependencies.iter().map(|(dep_name, dep_ref)| {
                    let dep_hash = match dep_ref {
                        DependencyRef::WorkspaceMember { .. } => {
                            panic!("unexpected workspace member reference for dependency {dep_name} in project {project_hash}");
                        },
                        DependencyRef::Project(hash) => *hash,
                    };
                    (dep_name.clone(), dep_hash)
                }).collect()
            },
        };
        Ok(dependencies)
    }

    fn workspace(&self, workspace_hash: WorkspaceHash) -> anyhow::Result<&Arc<Workspace>> {
        let workspace = self
            .workspaces
            .get(&workspace_hash)
            .with_context(|| format!("workspace not found for hash {workspace_hash}"))?;
        Ok(workspace)
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
        let project = self.project(project_hash)?;
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
        let project = self.project(project_hash)?;
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
        let project = self.project(project_hash)?;
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

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub definition: ProjectDefinition,
    pub dependencies: HashMap<String, DependencyRef>,
    pub modules: HashMap<RelativePathBuf, FileId>,
    #[serde_as(as = "HashMap<_, Vec<(_, _)>>")]
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub statics: HashMap<RelativePathBuf, BTreeMap<StaticQuery, Option<StaticOutput>>>,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum DependencyRef {
    WorkspaceMember {
        path: RelativePathBuf,
    },
    #[serde(untagged)]
    Project(ProjectHash),
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Workspace {
    members: BTreeMap<RelativePathBuf, Arc<Project>>,
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
pub struct WorkspaceHash(blake3::Hash);

impl WorkspaceHash {
    fn from_serializable<V>(value: &V) -> anyhow::Result<Self>
    where
        V: serde::Serialize,
    {
        let mut hasher = blake3::Hasher::new();

        json_canon::to_writer(&mut hasher, value)?;

        let hash = hasher.finalize();
        Ok(Self(hash))
    }

    pub fn validate_matches(&self, workspace: &Workspace) -> anyhow::Result<()> {
        let actual_hash = WorkspaceHash::from_serializable(workspace)?;
        anyhow::ensure!(
            self == &actual_hash,
            "workspace hash does not match expected hash"
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

impl std::fmt::Display for WorkspaceHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for WorkspaceHash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hash = blake3::Hash::from_hex(s)?;
        Ok(Self(hash))
    }
}

impl std::cmp::Ord for WorkspaceHash {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_bytes().cmp(other.0.as_bytes())
    }
}

impl std::cmp::PartialOrd for WorkspaceHash {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ProjectEntry {
    WorkspaceMember {
        workspace: WorkspaceHash,
        path: RelativePathBuf,
    },
    #[serde(untagged)]
    Project(Arc<Project>),
}

fn project_lockfile(project: &Project) -> Lockfile {
    let dependencies = project
        .dependencies
        .iter()
        .filter_map(|(name, dep_ref)| {
            let dep_hash = match dep_ref {
                DependencyRef::WorkspaceMember { .. } => {
                    return None;
                }
                DependencyRef::Project(hash) => *hash,
            };
            Some((name.clone(), dep_hash))
        })
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

fn workspace_definition(workspace: &Workspace) -> anyhow::Result<WorkspaceDefinition> {
    let members = workspace
        .members
        .keys()
        .map(|path| {
            let Some(last_component) = path.components().next_back() else {
                anyhow::bail!("invalid workspace member path: {path}");
            };

            let last_component = match last_component {
                relative_path::Component::CurDir | relative_path::Component::ParentDir => {
                    anyhow::bail!("invalid workspace member path: {path}");
                }
                relative_path::Component::Normal(component) => component,
            };

            // Shouldn't fail since we already validated the last component exists
            let path = path.parent().expect("parent path not found").to_owned();

            for component in path.components() {
                match component {
                    relative_path::Component::ParentDir => {
                        anyhow::bail!("invalid workspace member path: {path}");
                    }
                    relative_path::Component::CurDir => {}
                    relative_path::Component::Normal(component) => {
                        anyhow::ensure!(
                            !component.contains('*'),
                            "invalid wildcard in workspace member path: {path}"
                        );
                    }
                }
            }

            anyhow::ensure!(
                !last_component.contains('*'),
                "invlaid workspace member path: {path}"
            );
            Ok(WorkspaceMember::Path(path, last_component.to_string()))
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(WorkspaceDefinition { members })
}
