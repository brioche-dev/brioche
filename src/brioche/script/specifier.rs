use std::path::{Path, PathBuf};

use anyhow::Context as _;
use relative_path::{PathExt, RelativePathBuf};

use crate::brioche::{
    project::{find_project_root_sync, resolve_project},
    Brioche,
};

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

pub fn resolve(
    brioche: &Brioche,
    specifier: &str,
    referrer: &BriocheModuleSpecifier,
) -> anyhow::Result<BriocheModuleSpecifier> {
    match referrer {
        BriocheModuleSpecifier::Runtime { subpath } => {
            anyhow::ensure!(
                specifier.starts_with("./") || specifier.starts_with("../"),
                "invalid specifier {specifier:?} imported from {referrer}"
            );

            let new_subpath = subpath
                .parent()
                .map(|parent| parent.to_owned())
                .unwrap_or(RelativePathBuf::from(""))
                .join(specifier);

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

            anyhow::bail!("internal module {specifier:?} not found (imported from {referrer})");
        }
        BriocheModuleSpecifier::File { path } => {
            let project_root = find_project_root_sync(path)?;
            let subpath = path.relative_to(project_root)?;

            if specifier == "."
                || specifier == ".."
                || specifier.starts_with("./")
                || specifier.starts_with("../")
            {
                let new_subpath = subpath
                    .parent()
                    .map(|parent| parent.to_owned())
                    .unwrap_or(RelativePathBuf::from(""))
                    .join_normalized(specifier);

                let candidates = [new_subpath.join("brioche.bri"), new_subpath];

                for candidate in candidates {
                    let candidate_path = candidate.to_logical_path(project_root);
                    anyhow::ensure!(
                        candidate_path.starts_with(project_root),
                        "module {specifier:?} escapes project path {}",
                        project_root.display(),
                    );

                    if candidate_path.is_file() {
                        return Ok(BriocheModuleSpecifier::File {
                            path: candidate_path,
                        });
                    }
                }

                anyhow::bail!("module {specifier:?} not found (imported from {referrer})");
            } else if let Some(new_subpath) = specifier.strip_prefix('/') {
                let new_subpath = RelativePathBuf::from(new_subpath);
                let candidates = [new_subpath.join("brioche.bri"), new_subpath];

                for candidate in candidates {
                    let candidate_path = candidate.to_logical_path(project_root);
                    anyhow::ensure!(
                        candidate_path.starts_with(project_root),
                        "module {specifier:?} escapes project path {}",
                        project_root.display()
                    );

                    if candidate_path.is_file() {
                        return Ok(BriocheModuleSpecifier::File {
                            path: candidate_path,
                        });
                    }
                }

                anyhow::bail!("module {specifier:?} not found (imported from {referrer})");
            } else {
                let project = futures::executor::block_on(resolve_project(brioche, project_root))?;

                let dependency_project =
                    project.dependencies.get(specifier).with_context(|| {
                        format!("dependency {specifier:?} not found (imported from {referrer})")
                    })?;

                let dependency_path = dependency_project.local_path.join("brioche.bri");

                Ok(BriocheModuleSpecifier::File {
                    path: dependency_path,
                })
            }
        }
    }
}
