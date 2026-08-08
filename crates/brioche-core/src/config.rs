use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use tokio::io::AsyncReadExt as _;

pub async fn load_from_path(path: &Path) -> Result<Option<BriocheConfig>, LoadConfigError> {
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => {
            return Err(LoadConfigError::IoError {
                error,
                reason: format!("failed to open brioche config file '{}'", path.display()).into(),
            });
        }
    };

    let mut config_toml = String::new();
    file.read_to_string(&mut config_toml)
        .await
        .map_err(|error| LoadConfigError::IoError {
            error,
            reason: format!("failed to read brioche config file '{}'", path.display()).into(),
        })?;
    let config = toml::from_str::<BriocheConfig>(&config_toml).map_err(|error| {
        LoadConfigError::DeserializeError {
            error: Box::new(error),
            path: path.to_path_buf(),
        }
    })?;
    Ok(Some(config))
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct BriocheConfig {
    pub registry_url: Option<url::Url>,

    #[serde(default)]
    pub sandbox: SandboxConfig,

    pub cache: Option<CacheConfig>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "backend")]
#[serde(rename_all = "snake_case")]
pub enum SandboxConfig {
    #[default]
    Auto,
    LinuxNamespace(SandboxLinuxNamespaceConfig),
    Unsandboxed,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct SandboxLinuxNamespaceConfig {
    pub proot: Option<PRootConfig>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum PRootConfig {
    Auto(PRootAutoConfig),
    Value(bool),
    Custom { path: PathBuf },
}

impl Default for PRootConfig {
    fn default() -> Self {
        Self::Value(false)
    }
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PRootAutoConfig {
    #[default]
    Auto,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheConfig {
    pub url: url::Url,

    pub write_url: Option<url::Url>,

    #[serde(default = "default_cache_max_concurrent_operations")]
    pub max_concurrent_operations: usize,

    #[serde(default = "default_use_default_cache")]
    pub use_default_cache: bool,

    #[serde(default)]
    pub read_only: bool,

    pub allow_http: Option<bool>,

    #[serde(default, with = "humantime_serde")]
    pub timeout: Option<std::time::Duration>,

    #[serde(default, with = "humantime_serde")]
    pub connect_timeout: Option<std::time::Duration>,
}

const fn default_use_default_cache() -> bool {
    true
}

const fn default_cache_max_concurrent_operations() -> usize {
    200
}

#[derive(Debug, thiserror::Error)]
pub enum LoadConfigError {
    #[error("{reason}: {error}")]
    IoError {
        #[source]
        error: std::io::Error,
        reason: Cow<'static, str>,
    },
    #[error("error deserializing config at '{}': {error}", .path.display())]
    DeserializeError {
        #[source]
        error: Box<toml::de::Error>,
        path: PathBuf,
    },
}
