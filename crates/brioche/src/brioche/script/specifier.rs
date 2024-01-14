use std::{
    path::{Path, PathBuf},
    pin::Pin,
};

use anyhow::Context as _;
use relative_path::{PathExt, RelativePathBuf};

use crate::{
    brioche::{
        project::{find_project_root, find_project_root_sync, resolve_project},
        Brioche,
    },
    fs_utils::is_file,
};

/// A specifier from an `import` statement in a JavaScript module. Can
/// be resolved to a module specifier using the `resolve` function.
#[derive(Debug, Clone)]
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
            BriocheImportSpecifier::Local(specifier) => write!(f, "{}", specifier),
            BriocheImportSpecifier::External(specifier) => write!(f, "{}", specifier),
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
            BriocheLocalImportSpecifier::Relative(path) => write!(f, "{path}"),
            BriocheLocalImportSpecifier::ProjectRoot(path) => write!(f, "/{path}"),
        }
    }
}

/// An `import` specifier referring to a file within the current project.
#[derive(Debug, Clone)]
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
                    .map_err(|_| anyhow::anyhow!("failed to convert specifier {value} to path"))?;
                Ok(Self::File { path })
            }
            "briocheruntime" => {
                anyhow::ensure!(!value.has_host(), "invalid specifier: {}", value);
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
                let mut url: url::Url = "briocheruntime:///".parse().expect("failed to build URL");
                url.set_path(subpath.as_str());
                url
            }
            BriocheModuleSpecifier::File { path } => {
                url::Url::from_file_path(path).expect("failed to build URL")
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

pub async fn read_specifier_contents(
    specifier: &BriocheModuleSpecifier,
) -> anyhow::Result<Option<Pin<Box<dyn tokio::io::AsyncRead + Send + Sync>>>> {
    match specifier {
        BriocheModuleSpecifier::Runtime { subpath } => {
            let Some(file) = crate::brioche::RuntimeFiles::get(subpath.as_str()) else {
                return Ok(None);
            };
            let reader = std::io::Cursor::new(file.data);
            Ok(Some(Box::pin(reader)))
        }
        BriocheModuleSpecifier::File { path } => {
            let file = match tokio::fs::File::open(path).await {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).context("failed to open file"),
            };
            Ok(Some(Box::pin(file)))
        }
    }
}

pub fn read_specifier_contents_sync(
    specifier: &BriocheModuleSpecifier,
) -> anyhow::Result<Option<Box<dyn std::io::Read>>> {
    match specifier {
        BriocheModuleSpecifier::Runtime { subpath } => {
            let Some(file) = crate::brioche::RuntimeFiles::get(subpath.as_str()) else {
                return Ok(None);
            };
            let reader = std::io::Cursor::new(file.data);
            Ok(Some(Box::new(reader)))
        }
        BriocheModuleSpecifier::File { path } => {
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).context("failed to open file"),
            };
            Ok(Some(Box::new(file)))
        }
    }
}

pub async fn resolve(
    brioche: &Brioche,
    specifier: &BriocheImportSpecifier,
    referrer: &BriocheModuleSpecifier,
) -> anyhow::Result<BriocheModuleSpecifier> {
    match referrer {
        BriocheModuleSpecifier::Runtime { subpath } => {
            let specifier_path = match specifier {
                BriocheImportSpecifier::Local(BriocheLocalImportSpecifier::Relative(
                    specifier_path,
                )) => specifier_path,
                _ => {
                    anyhow::bail!("invalid specifier '{specifier}' imported from {referrer}");
                }
            };

            let new_subpath = subpath
                .parent()
                .map(|parent| parent.to_owned())
                .unwrap_or(RelativePathBuf::from(""))
                .join(specifier_path);

            let candidates = [
                new_subpath.join("index.js"),
                new_subpath.join("index.ts"),
                new_subpath.with_extension("js"),
                new_subpath.with_extension("ts"),
                new_subpath,
            ];

            for candidate in candidates {
                let file = crate::brioche::RuntimeFiles::get(candidate.as_str());
                if file.is_some() {
                    return Ok(BriocheModuleSpecifier::Runtime { subpath: candidate });
                }
            }

            anyhow::bail!("internal module '{specifier}' not found (imported from {referrer})");
        }
        BriocheModuleSpecifier::File { path } => {
            let project_root = find_project_root(path).await?;
            let subpath = path.relative_to(project_root)?;

            match specifier {
                BriocheImportSpecifier::Local(BriocheLocalImportSpecifier::Relative(
                    specifier_path,
                )) => {
                    let new_subpath = subpath
                        .parent()
                        .map(|parent| parent.to_owned())
                        .unwrap_or(RelativePathBuf::from(""))
                        .join_normalized(specifier_path);

                    let candidate_module_path = new_subpath.to_logical_path(project_root);
                    anyhow::ensure!(
                        candidate_module_path.starts_with(project_root),
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
                            candidate.starts_with(project_root),
                            "module '{specifier}' escapes project path {}",
                            project_root.display(),
                        );

                        if is_file(&candidate).await {
                            return Ok(BriocheModuleSpecifier::File { path: candidate });
                        }
                    }

                    anyhow::bail!("module '{specifier}' not found (imported from {referrer})");
                }
                BriocheImportSpecifier::Local(BriocheLocalImportSpecifier::ProjectRoot(
                    specifier_path,
                )) => {
                    let new_subpath = RelativePathBuf::from(specifier_path);

                    let candidate_module_path = new_subpath.to_logical_path(project_root);
                    anyhow::ensure!(
                        candidate_module_path.starts_with(project_root),
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
                            candidate.starts_with(project_root),
                            "module '{specifier}' escapes project path {}",
                            project_root.display(),
                        );

                        if is_file(&candidate).await {
                            return Ok(BriocheModuleSpecifier::File { path: candidate });
                        }
                    }

                    anyhow::bail!("module '{specifier}' not found (imported from {referrer})");
                }
                BriocheImportSpecifier::External(dep) => {
                    let project = resolve_project(brioche, project_root).await?;
                    let dependency_project = project.dependencies.get(dep).with_context(|| {
                        format!("dependency '{specifier}' not found (imported from {referrer})")
                    })?;

                    let dependency_path = dependency_project.local_path.join("project.bri");

                    Ok(BriocheModuleSpecifier::File {
                        path: dependency_path,
                    })
                }
            }
        }
    }
}

// TODO: Remove
pub fn resolve_sync(
    brioche: &Brioche,
    specifier: &BriocheImportSpecifier,
    referrer: &BriocheModuleSpecifier,
) -> anyhow::Result<BriocheModuleSpecifier> {
    match referrer {
        BriocheModuleSpecifier::Runtime { subpath } => {
            let specifier_path = match specifier {
                BriocheImportSpecifier::Local(BriocheLocalImportSpecifier::Relative(
                    specifier_path,
                )) => specifier_path,
                _ => {
                    anyhow::bail!("invalid specifier '{specifier}' imported from {referrer}");
                }
            };

            let new_subpath = subpath
                .parent()
                .map(|parent| parent.to_owned())
                .unwrap_or(RelativePathBuf::from(""))
                .join(specifier_path);

            let candidates = [
                new_subpath.join("index.js"),
                new_subpath.join("index.ts"),
                new_subpath.with_extension("js"),
                new_subpath.with_extension("ts"),
                new_subpath,
            ];

            for candidate in candidates {
                let file = crate::brioche::RuntimeFiles::get(candidate.as_str());
                if file.is_some() {
                    return Ok(BriocheModuleSpecifier::Runtime { subpath: candidate });
                }
            }

            anyhow::bail!("internal module '{specifier}' not found (imported from {referrer})");
        }
        BriocheModuleSpecifier::File { path } => {
            let project_root = find_project_root_sync(path)?;
            let subpath = path.relative_to(project_root)?;

            match specifier {
                BriocheImportSpecifier::Local(BriocheLocalImportSpecifier::Relative(
                    specifier_path,
                )) => {
                    let new_subpath = subpath
                        .parent()
                        .map(|parent| parent.to_owned())
                        .unwrap_or(RelativePathBuf::from(""))
                        .join_normalized(specifier_path);

                    let candidate_module_path = new_subpath.to_logical_path(project_root);
                    anyhow::ensure!(
                        candidate_module_path.starts_with(project_root),
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
                            candidate.starts_with(project_root),
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

                    let candidate_module_path = new_subpath.to_logical_path(project_root);
                    anyhow::ensure!(
                        candidate_module_path.starts_with(project_root),
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
                            candidate.starts_with(project_root),
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
                    let project =
                        futures::executor::block_on(resolve_project(brioche, project_root))?;
                    let dependency_project = project.dependencies.get(dep).with_context(|| {
                        format!("dependency '{specifier}' not found (imported from {referrer})")
                    })?;

                    let dependency_path = dependency_project.local_path.join("project.bri");

                    Ok(BriocheModuleSpecifier::File {
                        path: dependency_path,
                    })
                }
            }
        }
    }
}
