use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Context as _;

use super::Brioche;

#[async_recursion::async_recursion]
pub async fn resolve_project(brioche: &Brioche, path: &Path) -> anyhow::Result<Project> {
    tracing::debug!(path = %path.display(), "resolving project");

    let path = tokio::fs::canonicalize(path)
        .await
        .with_context(|| format!("failed to canonicalize path {}", path.display()))?;
    let repo = &brioche.repo_dir;

    let project_definition_path = path.join("brioche.toml");
    let project_definition = tokio::fs::read_to_string(&project_definition_path)
        .await
        .with_context(|| {
            format!(
                "failed to read project definition at {}",
                project_definition_path.display()
            )
        })?;
    let project_definition: ProjectDefinition =
        toml::from_str(&project_definition).with_context(|| {
            format!(
                "failed to parse project definition at {}",
                project_definition_path.display()
            )
        })?;

    let mut dependencies = HashMap::new();
    for (name, dependency_def) in &project_definition.dependencies {
        static NAME_REGEX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let name_regex = NAME_REGEX
            .get_or_init(|| regex::Regex::new("^[a-zA-Z0-9_]+$").expect("failed to compile regex"));
        anyhow::ensure!(name_regex.is_match(name), "invalid dependency name");

        let dependency = match dependency_def {
            DependencyDefinition::Path { path: subpath } => {
                let dep_path = path.join(subpath);
                resolve_project(brioche, &dep_path).await.with_context(|| {
                    format!(
                        "failed to resolve path dependency {name:?} in {}",
                        project_definition_path.display()
                    )
                })?
            }
            DependencyDefinition::Version(Version::Any) => {
                let local_path = repo.join(name);
                resolve_project(brioche, &local_path)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to resolve repo dependency {name:?} in {}",
                            project_definition_path.display()
                        )
                    })?
            }
        };

        dependencies.insert(name.to_owned(), dependency);
    }

    Ok(Project {
        local_path: path.to_owned(),
        dependencies,
    })
}

pub fn find_project_root_sync(path: &Path) -> anyhow::Result<&Path> {
    let mut current_path = path;
    loop {
        let project_definition_path = current_path.join("brioche.toml");
        if project_definition_path.exists() {
            return Ok(current_path);
        }

        current_path = match current_path.parent() {
            Some(parent) => parent,
            None => anyhow::bail!("project root not found"),
        };
    }
}

pub async fn find_project_root(path: &Path) -> anyhow::Result<&Path> {
    let mut current_path = path;
    loop {
        let project_definition_path = current_path.join("brioche.toml");
        let exists = tokio::fs::try_exists(&project_definition_path).await?;
        if exists {
            return Ok(current_path);
        }

        current_path = match current_path.parent() {
            Some(parent) => parent,
            None => anyhow::bail!("project root not found"),
        };
    }
}

#[derive(Debug, Clone)]
pub struct Project {
    pub local_path: PathBuf,
    pub dependencies: HashMap<String, Project>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectDefinition {
    #[serde(default)]
    pub dependencies: HashMap<String, DependencyDefinition>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum DependencyDefinition {
    Path { path: PathBuf },
    Version(Version),
}

#[derive(Debug, Clone, serde_with::DeserializeFromStr, serde_with::SerializeDisplay)]
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
