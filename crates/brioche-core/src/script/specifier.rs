use bstr::ByteSlice as _;

use crate::{BriocheState, path::RelativePath, project::ProjectRef};

pub fn resolve_import_specifier(
    brioche: &BriocheState,
    specifier: &ImportSpecifier,
    referrer: &ModuleSpecifier,
) -> Result<ModuleSpecifier, ResolveSpecifierError> {
    match referrer {
        ModuleSpecifier::Runtime {
            subpath_components: _,
        } => {
            let ImportSpecifier::Local(LocalImportSpecifier::Relative(_)) = specifier else {
                return Err(ResolveSpecifierError::InvalidRuntimeImport {
                    specifier: specifier.clone(),
                    referrer: referrer.clone(),
                });
            };

            // TODO: Import runtime files!

            // let new_subpath = subpath
            //     .parent()
            //     .map_or_else(|| RelativePathBuf::from(""), std::borrow::ToOwned::to_owned)
            //     .join(specifier_path);

            // let candidates = [
            //     new_subpath.join("index.js"),
            //     new_subpath.join("index.ts"),
            //     new_subpath.with_extension("js"),
            //     new_subpath.with_extension("ts"),
            //     new_subpath,
            // ];

            // for candidate in candidates {
            //     let file = crate::RuntimeFiles::get(candidate.as_str());
            //     if file.is_some() {
            //         return Ok(ModuleSpecifier::Runtime { subpath: candidate });
            //     }
            // }

            tracing::error!(
                ?specifier,
                ?referrer,
                "tried to resolve runtime specifier, which is not implemented!"
            );

            Err(ResolveSpecifierError::NotFound {
                specifier: specifier.clone(),
                referrer: referrer.clone(),
            })
        }
        ModuleSpecifier::File { path } => {
            let module = brioche
                .projects
                .module_by_path(path)
                .and_then(|module_ref| brioche.projects.project_by_module(module_ref));
            let Some((project_ref, referrer_subpath)) = module else {
                return Err(ResolveSpecifierError::ReferrerNotFound {
                    referrer: referrer.clone(),
                });
            };

            match specifier {
                ImportSpecifier::Local(LocalImportSpecifier::Relative(specifier_path)) => {
                    let new_subpath = referrer_subpath
                        .parent()
                        .unwrap_or_default()
                        .join(RelativePath::new(specifier_path))
                        .normalized_subpath()
                        .map_err(
                            |error| ResolveSpecifierError::RelativeImportEscapesProject {
                                error,
                                specifier_path: specifier_path.clone(),
                                referrer: referrer.clone(),
                                referrer_project_ref: *project_ref,
                                referrer_module_subpath: referrer_subpath.clone(),
                            },
                        )?;

                    let candidate_subpaths = if new_subpath.is_empty() {
                        vec![new_subpath.join_one("project.bri")]
                    } else {
                        vec![new_subpath.clone(), new_subpath.join_one("index.bri")]
                    };

                    for candidate_subpath in candidate_subpaths {
                        let Some(resolved_module_ref) = brioche
                            .projects
                            .project_module_at_subpath(*project_ref, &candidate_subpath)
                        else {
                            continue;
                        };

                        let path = brioche.projects.local_module_path(resolved_module_ref);
                        return Ok(ModuleSpecifier::File { path });
                    }

                    Err(ResolveSpecifierError::NotFound {
                        specifier: specifier.clone(),
                        referrer: referrer.clone(),
                    })
                }
                ImportSpecifier::Local(LocalImportSpecifier::ProjectRoot(specifier_path)) => {
                    let new_subpath = RelativePath::new(specifier_path)
                        .normalized_subpath()
                        .map_err(
                            |error| ResolveSpecifierError::RelativeImportEscapesProject {
                                error,
                                specifier_path: specifier_path.clone(),
                                referrer: referrer.clone(),
                                referrer_project_ref: *project_ref,
                                referrer_module_subpath: referrer_subpath.clone(),
                            },
                        )?;

                    let candidate_subpaths = if new_subpath.is_empty() {
                        vec![new_subpath.join_one("project.bri")]
                    } else {
                        vec![new_subpath.clone(), new_subpath.join_one("index.bri")]
                    };

                    for candidate_subpath in candidate_subpaths {
                        let Some(resolved_module_ref) = brioche
                            .projects
                            .project_module_at_subpath(*project_ref, &candidate_subpath)
                        else {
                            continue;
                        };

                        let path = brioche.projects.local_module_path(resolved_module_ref);
                        return Ok(ModuleSpecifier::File { path });
                    }

                    Err(ResolveSpecifierError::NotFound {
                        specifier: specifier.clone(),
                        referrer: referrer.clone(),
                    })
                }
                ImportSpecifier::External(dep) => {
                    let project_dependencies =
                        crate::project::get_dependencies(brioche, *project_ref);
                    let dependency_ref = project_dependencies.get(dep).ok_or_else(|| {
                        ResolveSpecifierError::DependencyNotFound {
                            specifier: specifier.clone(),
                            referrer: referrer.clone(),
                            referrer_project_ref: *project_ref,
                        }
                    })?;
                    let dependency_root_module_ref =
                        crate::project::get_root_module(brioche, *project_ref);

                    let path = dependency_root_module_ref.map_or_else(
                        || {
                            brioche
                                .projects
                                .local_project_path(*dependency_ref)
                                .join_one("project.bri")
                        },
                        |resolved_module_ref| {
                            brioche.projects.local_module_path(resolved_module_ref)
                        },
                    );
                    Ok(ModuleSpecifier::File { path })
                }
            }
        }
    }
}

/// A specifier from an `import` statement in a JavaScript module.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImportSpecifier {
    /// A local import.
    Local(LocalImportSpecifier),

    /// An external dependency. Example: `import "somedep";`
    External(String),
}

impl std::str::FromStr for ImportSpecifier {
    type Err = std::convert::Infallible;

    fn from_str(specifier: &str) -> Result<Self, Self::Err> {
        if specifier == "."
            || specifier == ".."
            || specifier.starts_with("./")
            || specifier.starts_with("../")
        {
            Ok(Self::Local(LocalImportSpecifier::Relative(
                specifier.to_string(),
            )))
        } else if let Some(project_root_subpath) = specifier.strip_prefix('/') {
            Ok(Self::Local(LocalImportSpecifier::ProjectRoot(
                project_root_subpath.to_string(),
            )))
        } else {
            Ok(Self::External(specifier.to_string()))
        }
    }
}

impl std::fmt::Display for ImportSpecifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(specifier) => write!(f, "{specifier}"),
            Self::External(specifier) => write!(f, "{specifier}"),
        }
    }
}

/// An `import` specifier referring to a file within the current project.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LocalImportSpecifier {
    /// An import relative to the current module. Example: `import "./foo.bri";`
    Relative(String),
    /// An import relative to the root of the project. Example: `import "/foo.bri`
    ProjectRoot(String),
}

impl std::fmt::Display for LocalImportSpecifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relative(path) => write!(f, "{path}"),
            Self::ProjectRoot(path) => write!(f, "/{path}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum StaticSpecifier {
    Include(StaticInclude),
    Glob { patterns: Vec<String> },
    Download { url: url::Url },
    GitRef(StaticGitRef),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "include")]
#[serde(rename_all = "snake_case")]
pub enum StaticInclude {
    File { path: String },
    Directory { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct StaticGitRef {
    pub repository: url::Url,

    #[serde(rename = "ref")]
    pub ref_: String,
}

/// URL scheme for modules served from the prebuilt runtime bundle.
pub const RUNTIME_SCHEME: &str = "briocheruntime";

/// A specifier for a Brioche module, either from the filesystem or
/// from the internal runtime package. A module specifier can be converted
/// from/to a URL.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, serde_with::DeserializeFromStr, serde_with::SerializeDisplay,
)]
pub enum ModuleSpecifier {
    Runtime { subpath_components: Vec<String> },
    File { path: crate::path::AbsolutePath },
}

impl ModuleSpecifier {
    #[must_use]
    pub fn from_path(path: &crate::path::AbsolutePath) -> Self {
        Self::File { path: path.clone() }
    }
}

impl TryFrom<&'_ url::Url> for ModuleSpecifier {
    type Error = ModuleSpecifierParseError;

    fn try_from(url: &url::Url) -> Result<Self, Self::Error> {
        match url.scheme() {
            "file" => {
                let path = url
                    .to_file_path()
                    .ok()
                    .and_then(|path| crate::path::from_absolute_system_path(&path).ok())
                    .ok_or_else(|| {
                        ModuleSpecifierParseError::InvalidFilePath(Box::new(url.clone()))
                    })?;
                Ok(Self::File { path })
            }
            RUNTIME_SCHEME => {
                if url.has_host() {
                    return Err(ModuleSpecifierParseError::RuntimeSpecifierCannotHaveHost(
                        Box::new(url.clone()),
                    ));
                }
                let subpath = crate::path::RelativePath::new(url.path().trim_start_matches('/'));
                let subpath = subpath.normalized_subpath().map_err(|error| {
                    ModuleSpecifierParseError::RuntimeSpecifierInvalidSubpath {
                        url: Box::new(url.clone()),
                        error,
                    }
                })?;
                let subpath_components = subpath.components().map(|component| {
                    let crate::path::RelativePathComponent::Normal(component) = component else {
                        unreachable!("encountered non-normal path component after normalizing subpath '{subpath}'");
                    };

                    component.to_str().unwrap_or_else(|error| panic!("encountered non-UTF-8 component in subpath '{subpath}': {error}")).to_string()
                }).collect();
                Ok(Self::Runtime { subpath_components })
            }
            scheme => Err(ModuleSpecifierParseError::UnsupportedScheme {
                scheme: scheme.to_string(),
                url: Box::new(url.clone()),
            }),
        }
    }
}

impl TryFrom<url::Url> for ModuleSpecifier {
    type Error = ModuleSpecifierParseError;

    fn try_from(value: url::Url) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

impl TryFrom<&'_ ModuleSpecifier> for url::Url {
    type Error = ModuleSpecifierToUrlError;

    fn try_from(value: &ModuleSpecifier) -> Result<Self, Self::Error> {
        match value {
            ModuleSpecifier::Runtime { subpath_components } => {
                let mut url: Self = format!("{RUNTIME_SCHEME}:///")
                    .parse()
                    .expect("failed to parse runtime module URL");
                url.path_segments_mut()
                    .expect("failed to get URL path segments")
                    .pop_if_empty()
                    .extend(subpath_components);
                Ok(url)
            }
            ModuleSpecifier::File { path } => {
                let system_path = path.to_system_path()?;
                let url = Self::from_file_path(system_path)
                    .map_err(|()| ModuleSpecifierToUrlError::UrlFromPath(path.clone()))?;
                Ok(url)
            }
        }
    }
}

impl TryFrom<ModuleSpecifier> for url::Url {
    type Error = ModuleSpecifierToUrlError;

    fn try_from(value: ModuleSpecifier) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

impl std::str::FromStr for ModuleSpecifier {
    type Err = ModuleSpecifierParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let url: url::Url =
            s.parse()
                .map_err(|error| ModuleSpecifierParseError::UrlParseError {
                    error,
                    url: s.to_string(),
                })?;
        let specifier = url.try_into()?;
        Ok(specifier)
    }
}

impl std::fmt::Display for ModuleSpecifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match url::Url::try_from(self) {
            Ok(url) => write!(f, "{url}"),
            Err(_) => write!(f, "(invalid specifier: {self:?})"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveSpecifierError {
    #[error("module '{specifier}' not found (referred from '{referrer}')")]
    NotFound {
        specifier: ImportSpecifier,
        referrer: ModuleSpecifier,
    },

    #[error("dependency '{specifier}' not found (referred from '{referrer}')")]
    DependencyNotFound {
        specifier: ImportSpecifier,
        referrer: ModuleSpecifier,
        referrer_project_ref: ProjectRef,
    },

    #[error("failed to find referrer module '{referrer}'")]
    ReferrerNotFound { referrer: ModuleSpecifier },

    #[error("cannot import module '{specifier} from '{referrer}'")]
    InvalidRuntimeImport {
        specifier: ImportSpecifier,
        referrer: ModuleSpecifier,
    },

    #[error(
        "specifier '{specifier_path}' included from '{referrer_module_subpath}' escapes project root"
    )]
    RelativeImportEscapesProject {
        #[source]
        error: crate::path::SubpathError,
        specifier_path: String,
        referrer: ModuleSpecifier,
        referrer_project_ref: ProjectRef,
        referrer_module_subpath: RelativePath,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleSpecifierParseError {
    #[error("error parsing URL '{url}': {error}")]
    UrlParseError {
        #[source]
        error: url::ParseError,
        url: String,
    },

    #[error("invalid path in file:// URL '{0}'")]
    InvalidFilePath(Box<url::Url>),

    #[error("{RUNTIME_SCHEME}:// URL '{0}' cannot have a host")]
    RuntimeSpecifierCannotHaveHost(Box<url::Url>),

    #[error("invalid subpath in URL '{url}': {error}")]
    RuntimeSpecifierInvalidSubpath {
        #[source]
        error: crate::path::SubpathError,
        url: Box<url::Url>,
    },

    #[error("scheme '{scheme}' is unsupported in URL '{url}'")]
    UnsupportedScheme { scheme: String, url: Box<url::Url> },
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleSpecifierToUrlError {
    #[error(transparent)]
    ToSystemPath(#[from] crate::path::ToSystemPathError),

    #[error("failed to build module specifier URL from path '{0}'")]
    UrlFromPath(crate::path::AbsolutePath),
}
