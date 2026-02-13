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
