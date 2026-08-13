use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::RwLock;

pub mod blob;
pub mod cache;
pub mod config;
mod download;
mod encoding;
mod fs_utils;
mod hash;
mod object_store_utils;
pub mod path;
pub mod platform;
pub mod project;
pub mod recipe;
pub mod registry;
pub mod reporter;
pub mod script;
mod utils;

const MAX_CONCURRENT_DOWNLOADS: usize = 20;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!("brioche/", env!("CARGO_PKG_VERSION"));

static DEFAULT_REGISTRY_URL: std::sync::LazyLock<url::Url> =
    std::sync::LazyLock::new(|| "https://registry.brioche.dev/".parse().unwrap());

#[derive(Clone)]
pub struct Brioche {
    resources: Arc<BriocheResources>,
    state: Arc<RwLock<BriocheState>>,
}

impl Brioche {
    #[must_use]
    pub const fn builder() -> BriocheBuilder {
        BriocheBuilder {
            reporter: None,
            config: None,
            data_dir: None,
            cache_client: None,
            registry_client: None,
        }
    }

    #[must_use]
    pub async fn new() -> Self {
        Self::builder().build().await.unwrap()
    }

    /// The directory where all of Brioche's data is stored. Usually configured
    /// to follow the platform's conventions for storing application data, such
    /// as `~/.local/share/brioche` on Linux.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.resources.data_dir
    }

    pub async fn read(&self) -> BriocheRef<'_> {
        BriocheRef(self.state.read().await)
    }

    pub async fn write(&self) -> BriocheMut<'_> {
        BriocheMut(self.state.write().await)
    }

    #[must_use]
    pub const fn resources(&self) -> &Arc<BriocheResources> {
        &self.resources
    }
}

pub struct BriocheResources {
    reporter: reporter::Reporter,

    data_dir: PathBuf,

    registry_client: registry::RegistryClient,

    cache_client: cache::CacheClient,

    download_semaphore: tokio::sync::Semaphore,

    download_client: reqwest_middleware::ClientWithMiddleware,
}

pub struct BriocheRef<'a>(tokio::sync::RwLockReadGuard<'a, BriocheState>);

impl std::ops::Deref for BriocheRef<'_> {
    type Target = BriocheState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct BriocheMut<'a>(tokio::sync::RwLockWriteGuard<'a, BriocheState>);

impl std::ops::Deref for BriocheMut<'_> {
    type Target = BriocheState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for BriocheMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub struct BriocheState {
    resources: Arc<BriocheResources>,
    projects: project::Projects,
    recipes: recipe::Recipes,
}

impl BriocheState {
    #[must_use]
    pub const fn resources(&self) -> &Arc<BriocheResources> {
        &self.resources
    }
}

pub struct BriocheBuilder {
    reporter: Option<reporter::Reporter>,
    config: Option<config::BriocheConfig>,
    data_dir: Option<PathBuf>,
    cache_client: Option<cache::CacheClient>,
    registry_client: Option<registry::RegistryClient>,
}

impl BriocheBuilder {
    #[must_use]
    pub fn reporter(mut self, reporter: reporter::Reporter) -> Self {
        self.reporter = Some(reporter);
        self
    }

    #[must_use]
    pub fn config(mut self, config: config::BriocheConfig) -> Self {
        self.config = Some(config);
        self
    }

    #[must_use]
    pub fn data_dir(mut self, data_dir: impl AsRef<Path>) -> Self {
        self.data_dir = Some(data_dir.as_ref().to_path_buf());
        self
    }

    #[must_use]
    pub fn cache_client(mut self, cache_client: cache::CacheClient) -> Self {
        self.cache_client = Some(cache_client);
        self
    }

    #[must_use]
    pub fn registry_client(mut self, registry_client: registry::RegistryClient) -> Self {
        self.registry_client = Some(registry_client);
        self
    }

    pub async fn build(self) -> Result<Brioche, BuildBriocheError> {
        let dirs = directories::ProjectDirs::from("dev", "brioche", "brioche")
            .ok_or(BuildBriocheError::FailedToGetDirs)?;
        let config = if let Some(config) = self.config {
            config
        } else {
            let config_path = dirs.config_dir().join("config.toml");
            let config = config::load_from_path(&config_path).await?;
            config.unwrap_or_default()
        };

        let reporter = self.reporter.unwrap_or_else(|| {
            let (reporter, _) = reporter::start_null_reporter();
            reporter
        });

        let data_dir = match (self.data_dir, std::env::var_os("BRIOCHE_DATA_DIR")) {
            (Some(data_dir), _) => data_dir,
            (None, Some(data_dir)) => PathBuf::from(data_dir),
            (None, None) => dirs.data_local_dir().to_owned(),
        };

        let cache_client = if let Some(cache_client) = self.cache_client {
            cache_client
        } else {
            let cache_config = match std::env::var_os("BRIOCHE_CACHE_URL") {
                Some(url) => {
                    let url = url
                        .to_str()
                        .ok_or_else(|| BuildBriocheError::InvalidEnvValue {
                            env: "BRIOCHE_CACHE_URL".into(),
                            reason: "invalid UTF-8".into(),
                            error: None,
                        })?;
                    let url = url
                        .parse()
                        .map_err(|error| BuildBriocheError::InvalidEnvValue {
                            env: "BRIOCHE_CACHE_URL".into(),
                            reason: "invalid URL".into(),
                            error: Some(Box::new(error)),
                        })?;
                    let write_url = std::env::var_os("BRIOCHE_CACHE_WRITE_URL")
                        .map(|write_url| {
                            let write_url = write_url.to_str().ok_or_else(|| {
                                BuildBriocheError::InvalidEnvValue {
                                    env: "BRIOCHE_CACHE_WRITE_URL".into(),
                                    reason: "invalid UTF-8".into(),
                                    error: None,
                                }
                            })?;
                            let write_url = write_url.parse().map_err(|error| {
                                BuildBriocheError::InvalidEnvValue {
                                    env: "BRIOCHE_CACHE_WRITE_URL".into(),
                                    reason: "invalid URL".into(),
                                    error: Some(Box::new(error)),
                                }
                            })?;
                            Ok::<_, BuildBriocheError>(write_url)
                        })
                        .transpose()?;
                    let use_default_cache =
                        match std::env::var_os("BRIOCHE_CACHE_USE_DEFAULT_CACHE") {
                            Some(value) if value.to_str() == Some("true") => true,
                            Some(value) if value.to_str() == Some("false") => false,
                            Some(_) => {
                                return Err(BuildBriocheError::InvalidEnvValue {
                                    env: "BRIOCHE_CACHE_USE_DEFAULT_CACHE".into(),
                                    reason: "expected 'true' or 'false'".into(),
                                    error: None,
                                });
                            }
                            None => true,
                        };
                    let read_only = match std::env::var_os("BRIOCHE_CACHE_READ_ONLY") {
                        Some(value) if value.to_str() == Some("true") => true,
                        Some(value) if value.to_str() == Some("false") => false,
                        Some(_) => {
                            return Err(BuildBriocheError::InvalidEnvValue {
                                env: "BRIOCHE_CACHE_READ_ONLY".into(),
                                reason: "expected 'true' or 'false'".into(),
                                error: None,
                            });
                        }
                        None => false,
                    };
                    let max_concurrent_operations =
                        match std::env::var_os("BRIOCHE_CACHE_MAX_CONCURRENT_OPERATIONS") {
                            Some(value) => {
                                let value = value.to_str().ok_or_else(|| {
                                    BuildBriocheError::InvalidEnvValue {
                                        env: "BRIOCHE_CACHE_MAX_CONCURRENT_OPERATIONS".into(),
                                        reason: "invalid UTF-8".into(),
                                        error: None,
                                    }
                                })?;
                                let value: usize = value.parse().map_err(|error| {
                                    BuildBriocheError::InvalidEnvValue {
                                        env: "BRIOCHE_CACHE_MAX_CONCURRENT_OPERATIONS".into(),
                                        reason: "failed to parse".into(),
                                        error: Some(Box::new(error)),
                                    }
                                })?;
                                value
                            }
                            None => cache::DEFAULT_CACHE_MAX_CONCURRENT_OPERATIONS,
                        };
                    let allow_http = match std::env::var_os("BRIOCHE_CACHE_ALLOW_HTTP") {
                        Some(value) if value.to_str() == Some("true") => Some(true),
                        Some(value) if value.to_str() == Some("false") => Some(false),
                        Some(_) => {
                            return Err(BuildBriocheError::InvalidEnvValue {
                                env: "BRIOCHE_CACHE_ALLOW_HTTP".into(),
                                reason: "expected 'true' or 'false'".into(),
                                error: None,
                            });
                        }
                        None => None,
                    };
                    let timeout = std::env::var_os("BRIOCHE_CACHE_TIMEOUT")
                        .map(|value| {
                            let value = value.to_str().ok_or_else(|| {
                                BuildBriocheError::InvalidEnvValue {
                                    env: "BRIOCHE_CACHE_TIMEOUT".into(),
                                    reason: "invalid UTF-8".into(),
                                    error: None,
                                }
                            })?;
                            let duration = humantime::parse_duration(value).map_err(|error| {
                                BuildBriocheError::InvalidEnvValue {
                                    env: "BRIOCHE_CACHE_TIMEOUT".into(),
                                    reason: "invalid duration".into(),
                                    error: Some(Box::new(error)),
                                }
                            })?;
                            Ok::<_, BuildBriocheError>(duration)
                        })
                        .transpose()?;
                    let connect_timeout = std::env::var_os("BRIOCHE_CACHE_CONNECT_TIMEOUT")
                        .map(|value| {
                            let value = value.to_str().ok_or_else(|| {
                                BuildBriocheError::InvalidEnvValue {
                                    env: "BRIOCHE_CACHE_CONNECT_TIMEOUT".into(),
                                    reason: "invalid UTF-8".into(),
                                    error: None,
                                }
                            })?;
                            let duration = humantime::parse_duration(value).map_err(|error| {
                                BuildBriocheError::InvalidEnvValue {
                                    env: "BRIOCHE_CACHE_CONNECT_TIMEOUT".into(),
                                    reason: "invalid duration".into(),
                                    error: Some(Box::new(error)),
                                }
                            })?;
                            Ok::<_, BuildBriocheError>(duration)
                        })
                        .transpose()?;
                    Some(config::CacheConfig {
                        url,
                        write_url,
                        max_concurrent_operations,
                        use_default_cache,
                        read_only,
                        allow_http,
                        timeout,
                        connect_timeout,
                    })
                }
                None => config.cache.clone(),
            };
            cache::cache_client_with_config(cache_config.as_ref()).await?
        };

        let registry_client = self.registry_client.unwrap_or_else(|| {
            registry::RegistryClient::new(registry::RegistryClientConfig::new(
                DEFAULT_REGISTRY_URL.clone(),
            ))
        });

        let download_retry_policy = reqwest_retry::policies::ExponentialBackoff::builder()
            .retry_bounds(
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(30),
            )
            .build_with_max_retries(5);
        let download_retry_middleware =
            reqwest_retry::RetryTransientMiddleware::new_with_policy(download_retry_policy);
        let download_client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .pool_idle_timeout(std::time::Duration::from_mins(1))
            .pool_max_idle_per_host(10)
            .build()?;
        let download_client = reqwest_middleware::ClientBuilder::new(download_client)
            .with(download_retry_middleware)
            .build();

        let resources = Arc::new(BriocheResources {
            reporter,
            data_dir,
            registry_client,
            cache_client,
            download_semaphore: tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS),
            download_client,
        });
        let state = Arc::new(RwLock::new(BriocheState {
            resources: resources.clone(),
            projects: project::Projects::default(),
            recipes: recipe::Recipes::default(),
        }));

        Ok(Brioche { resources, state })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildBriocheError {
    #[error("failed to get Brioche directories (is $HOME set?)")]
    FailedToGetDirs,

    #[error("invalid value for ${env}: {reason}")]
    InvalidEnvValue {
        env: Cow<'static, str>,
        reason: Cow<'static, str>,

        #[source]
        error: Option<Box<dyn std::error::Error>>,
    },

    #[error(transparent)]
    LoadConfigError(#[from] config::LoadConfigError),

    #[error(transparent)]
    CacheError(#[from] cache::CacheError),

    #[error(transparent)]
    ReqwestError(#[from] reqwest::Error),
}
