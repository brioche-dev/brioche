use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use relative_path::{PathExt as _, RelativePathBuf};

use crate::{project::Projects, vfs::Vfs};

/// A specifier from an `import` statement in a JavaScript module. Can
/// be resolved to a module specifier using the `resolve` function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BriocheImportSpecifier {
    /// A local import.
    Local(BriocheLocalImportSpecifier),

    /// An external dependency. Example: `import "somedep";`
    External(String),
}

impl std::str::FromStr for BriocheImportSpecifier {
    type Err = anyhow::Error;

    fn from_str(specifier: &str) -> Result<Self, Self::Err> {
        if specifier == "."
            || specifier == ".."
            || specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier.starts_with('/')
        {
            let local_specifier = specifier.parse()?;
            Ok(Self::Local(local_specifier))
        } else {
            Ok(Self::External(specifier.to_string()))
        }
    }
}

impl std::fmt::Display for BriocheImportSpecifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(specifier) => write!(f, "{specifier}"),
            Self::External(specifier) => write!(f, "{specifier}"),
        }
    }
}

impl std::str::FromStr for BriocheLocalImportSpecifier {
    type Err = anyhow::Error;

    fn from_str(specifier: &str) -> Result<Self, Self::Err> {
        if specifier == "."
            || specifier == ".."
            || specifier.starts_with("./")
            || specifier.starts_with("../")
        {
            Ok(Self::Relative(specifier.to_string()))
        } else if let Some(project_root_subpath) = specifier.strip_prefix('/') {
            Ok(Self::ProjectRoot(project_root_subpath.to_string()))
        } else {
            anyhow::bail!("invlaid local import specifier: {specifier}");
        }
    }
}

impl std::fmt::Display for BriocheLocalImportSpecifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relative(path) => write!(f, "{path}"),
            Self::ProjectRoot(path) => write!(f, "/{path}"),
        }
    }
}

/// An `import` specifier referring to a file within the current project.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BriocheLocalImportSpecifier {
    /// An import relative to the current module. Example: `import "./foo.bri";`
    Relative(String),
    /// An import relative to the root of the project. Example: `import "/foo.bri`
    ProjectRoot(String),
}

/// A specifier for a Brioche module, either from the filesystem or
/// from the internal runtime package. A module specifier can be converted
/// from/to a URL.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, serde_with::DeserializeFromStr, serde_with::SerializeDisplay,
)]
pub enum BriocheModuleSpecifier {
    Runtime { subpath: RelativePathBuf },
    File { path: PathBuf },
}

impl BriocheModuleSpecifier {
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        Self::File {
            path: path.to_owned(),
        }
    }
}

impl TryFrom<&'_ url::Url> for BriocheModuleSpecifier {
    type Error = anyhow::Error;

    fn try_from(value: &url::Url) -> Result<Self, Self::Error> {
        match value.scheme() {
            "file" => {
                let path = value
                    .to_file_path()
                    .map_err(|()| anyhow::anyhow!("failed to convert specifier {value} to path"))?;
                Ok(Self::File { path })
            }
            "briocheruntime" => {
                anyhow::ensure!(!value.has_host(), "invalid specifier: {value}");
                let subpath = RelativePathBuf::from(value.path().trim_start_matches('/'));
                Ok(Self::Runtime { subpath })
            }
            _ => {
                anyhow::bail!("invalid scheme in specifier: {value}");
            }
        }
    }
}

impl TryFrom<url::Url> for BriocheModuleSpecifier {
    type Error = anyhow::Error;

    fn try_from(value: url::Url) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

impl From<&'_ BriocheModuleSpecifier> for url::Url {
    fn from(value: &BriocheModuleSpecifier) -> Self {
        match value {
            BriocheModuleSpecifier::Runtime { subpath } => {
                let mut url: Self = "briocheruntime:///".parse().expect("failed to build URL");
                url.set_path(subpath.as_str());
                url
            }
            BriocheModuleSpecifier::File { path } => {
                Self::from_file_path(path).expect("failed to build URL")
            }
        }
    }
}

impl From<BriocheModuleSpecifier> for url::Url {
    fn from(value: BriocheModuleSpecifier) -> Self {
        Self::from(&value)
    }
}

impl std::str::FromStr for BriocheModuleSpecifier {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let url: url::Url = s.parse()?;
        let specifier = url.try_into()?;
        Ok(specifier)
    }
}

impl std::fmt::Display for BriocheModuleSpecifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", url::Url::from(self))
    }
}

pub fn runtime_specifiers_with_contents()
-> impl Iterator<Item = (BriocheModuleSpecifier, Cow<'static, [u8]>)> {
    crate::RuntimeFiles::iter().filter_map(|path| {
        let file = crate::RuntimeFiles::get(&path)?;
        let specifier = BriocheModuleSpecifier::Runtime {
            subpath: RelativePathBuf::from(&*path),
        };
        Some((specifier, file.data))
    })
}

pub fn read_specifier_contents(
    vfs: &Vfs,
    specifier: &BriocheModuleSpecifier,
) -> anyhow::Result<Arc<Vec<u8>>> {
    match specifier {
        BriocheModuleSpecifier::Runtime { subpath } => {
            let file = crate::RuntimeFiles::get(subpath.as_str())
                .with_context(|| format!("internal module '{specifier}' not found"))?;
            Ok(Arc::new(file.data.to_vec()))
        }
        BriocheModuleSpecifier::File { path } => {
            let (_, contents) = vfs
                .load_cached(path)?
                .with_context(|| format!("module '{specifier}' not loaded"))?;
            Ok(contents)
        }
    }
}

pub async fn load_specifier_contents(
    vfs: &Vfs,
    specifier: &BriocheModuleSpecifier,
) -> anyhow::Result<Arc<Vec<u8>>> {
    match specifier {
        BriocheModuleSpecifier::Runtime { .. } => {}
        BriocheModuleSpecifier::File { path } => {
            vfs.load(path)
                .await
                .with_context(|| format!("failed to load module '{specifier}'"))?;
        }
    }

    read_specifier_contents(vfs, specifier)
}

pub fn resolve(
    projects: &Projects,
    specifier: &BriocheImportSpecifier,
    referrer: &BriocheModuleSpecifier,
) -> anyhow::Result<BriocheModuleSpecifier> {
    match referrer {
        BriocheModuleSpecifier::Runtime { subpath } => {
            let BriocheImportSpecifier::Local(BriocheLocalImportSpecifier::Relative(
                specifier_path,
            )) = specifier
            else {
                anyhow::bail!("invalid specifier '{specifier}' imported from {referrer}");
            };

            let new_subpath = subpath
                .parent()
                .map_or_else(|| RelativePathBuf::from(""), std::borrow::ToOwned::to_owned)
                .join(specifier_path);

            let candidates = [
                new_subpath.join("index.js"),
                new_subpath.join("index.ts"),
                new_subpath.with_extension("js"),
                new_subpath.with_extension("ts"),
                new_subpath,
            ];

            for candidate in candidates {
                let file = crate::RuntimeFiles::get(candidate.as_str());
                if file.is_some() {
                    return Ok(BriocheModuleSpecifier::Runtime { subpath: candidate });
                }
            }

            anyhow::bail!("internal module '{specifier}' not found (imported from {referrer})");
        }
        BriocheModuleSpecifier::File { path } => {
            let project_hash = projects.find_containing_project(path)?.with_context(|| {
                format!(
                    "project containing module '{specifier}' not found (imported from {referrer})"
                )
            })?;
            let project_root = projects.find_containing_project_root(path, project_hash)?;
            let subpath = path.relative_to(&project_root)?;

            match specifier {
                BriocheImportSpecifier::Local(BriocheLocalImportSpecifier::Relative(
                    specifier_path,
                )) => {
                    let new_subpath = subpath
                        .parent()
                        .map_or_else(|| RelativePathBuf::from(""), std::borrow::ToOwned::to_owned)
                        .join_normalized(specifier_path);

                    let candidate_module_path = new_subpath.to_logical_path(&project_root);
                    anyhow::ensure!(
                        candidate_module_path.starts_with(&project_root),
                        "module '{specifier}' escapes project path {}",
                        project_root.display(),
                    );

                    let candidates = if candidate_module_path == *project_root {
                        vec![candidate_module_path.join("project.bri")]
                    } else {
                        vec![
                            candidate_module_path.clone(),
                            candidate_module_path.join("index.bri"),
                        ]
                    };

                    for candidate in candidates {
                        anyhow::ensure!(
                            candidate.starts_with(&project_root),
                            "module '{specifier}' escapes project path {}",
                            project_root.display(),
                        );

                        if candidate.is_file() {
                            return Ok(BriocheModuleSpecifier::File { path: candidate });
                        }
                    }

                    anyhow::bail!("module '{specifier}' not found (imported from {referrer})");
                }
                BriocheImportSpecifier::Local(BriocheLocalImportSpecifier::ProjectRoot(
                    specifier_path,
                )) => {
                    let new_subpath = RelativePathBuf::from(specifier_path);

                    let candidate_module_path = new_subpath.to_logical_path(&project_root);
                    anyhow::ensure!(
                        candidate_module_path.starts_with(&project_root),
                        "module '{specifier}' escapes project path {}",
                        project_root.display(),
                    );

                    let candidates = if candidate_module_path == *project_root {
                        vec![candidate_module_path.join("project.bri")]
                    } else {
                        vec![
                            candidate_module_path.clone(),
                            candidate_module_path.join("index.bri"),
                        ]
                    };

                    for candidate in candidates {
                        anyhow::ensure!(
                            candidate.starts_with(&project_root),
                            "module '{specifier}' escapes project path {}",
                            project_root.display(),
                        );

                        if candidate.is_file() {
                            return Ok(BriocheModuleSpecifier::File { path: candidate });
                        }
                    }

                    anyhow::bail!("module '{specifier}' not found (imported from {referrer})");
                }
                BriocheImportSpecifier::External(dep) => {
                    let project_dependencies = projects.project_dependencies(project_hash)?;
                    let dependency_project_hash =
                        project_dependencies.get(dep).with_context(|| {
                            format!("dependency '{specifier}' not found (imported from {referrer})")
                        })?;

                    let dependency_root_module_specifier =
                        projects.project_root_module_specifier(*dependency_project_hash)?;
                    Ok(dependency_root_module_specifier)
                }
            }
        }
    }
}
