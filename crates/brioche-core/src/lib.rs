use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
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
pub mod projects;
pub mod recipe;
pub mod registry;
pub mod reporter;
mod script;
mod utils;

const MAX_CONCURRENT_DOWNLOADS: usize = 20;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!("brioche/", env!("CARGO_PKG_VERSION"));

static DEFAULT_REGISTRY_URL: std::sync::LazyLock<url::Url> =
    std::sync::LazyLock::new(|| "https://registry.brioche.dev/".parse().unwrap());

#[derive(Clone)]
pub struct Brioche {
    reporter: reporter::Reporter,
    projects: Arc<RwLock<projects::Projects>>,
    recipes: Arc<RwLock<recipe::Recipes>>,

    /// The directory where all of Brioche's data is stored. Usually configured
    /// to follow the platform's conventions for storing application data, such
    /// as `~/.local/share/brioche` on Linux.
    pub data_dir: PathBuf,

    registry_client: registry::RegistryClient,

    pub cache_client: cache::CacheClient,

    pub download_semaphore: Arc<tokio::sync::Semaphore>,

    pub download_client: reqwest_middleware::ClientWithMiddleware,
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

    #[must_use]
    pub const fn recipes(&self) -> &Arc<RwLock<recipe::Recipes>> {
        &self.recipes
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

    pub async fn build(self) -> anyhow::Result<Brioche> {
        let dirs = directories::ProjectDirs::from("dev", "brioche", "brioche")
            .context("failed to get Brioche directories (is $HOME set?)")?;
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
                    let url = url.to_str().ok_or_else(|| {
                        anyhow::anyhow!("invalid URL for $BRIOCHE_CACHE_URL: {}", url.display())
                    })?;
                    let url = url
                        .parse()
                        .with_context(|| format!("invalid URL for $BRIOCHE_CACHE_URL: {url:?}"))?;
                    let write_url = std::env::var_os("BRIOCHE_CACHE_WRITE_URL")
                        .map(|write_url| {
                            let write_url = write_url.to_str().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "invalid URL for $BRIOCHE_CACHE_WRITE_URL: {}",
                                    write_url.display()
                                )
                            })?;
                            let write_url = write_url.parse().with_context(|| {
                                format!("invalid URL for $BRIOCHE_CACHE_WRITE_URL: {write_url:?}")
                            })?;
                            anyhow::Ok(write_url)
                        })
                        .transpose()?;
                    let use_default_cache =
                        match std::env::var_os("BRIOCHE_CACHE_USE_DEFAULT_CACHE") {
                            Some(value) if value.to_str() == Some("true") => true,
                            Some(value) if value.to_str() == Some("false") => false,
                            Some(value) => {
                                anyhow::bail!(
                                    "invalid value for $BRIOCHE_CACHE_USE_DEFAULT_CACHE: {}",
                                    value.display()
                                );
                            }
                            None => true,
                        };
                    let read_only = match std::env::var_os("BRIOCHE_CACHE_READ_ONLY") {
                        Some(value) if value.to_str() == Some("true") => true,
                        Some(value) if value.to_str() == Some("false") => false,
                        Some(value) => {
                            anyhow::bail!(
                                "invalid value for $BRIOCHE_CACHE_READ_ONLY: {}",
                                value.display()
                            );
                        }
                        None => false,
                    };
                    let max_concurrent_operations = match std::env::var_os(
                        "BRIOCHE_CACHE_MAX_CONCURRENT_OPERATIONS",
                    ) {
                        Some(value) => {
                            let value = value.to_str().ok_or_else(|| anyhow::anyhow!("invalid value for $BRIOCHE_CACHE_MAX_CONCURRENT_OPERATIONS: {}", value.display()))?;
                            let value: usize = value.parse().with_context(|| format!("invalid value for $BRIOCHE_CACHE_MAX_CONCURRENT_OPERATIONS: {value:?}"))?;
                            value
                        }
                        None => cache::DEFAULT_CACHE_MAX_CONCURRENT_OPERATIONS,
                    };
                    let allow_http = match std::env::var_os("BRIOCHE_CACHE_ALLOW_HTTP") {
                        Some(value) if value.to_str() == Some("true") => Some(true),
                        Some(value) if value.to_str() == Some("false") => Some(false),
                        Some(value) => {
                            anyhow::bail!(
                                "invalid value for $BRIOCHE_CACHE_ALLOW_HTTP: {}",
                                value.display()
                            );
                        }
                        None => None,
                    };
                    let timeout = std::env::var_os("BRIOCHE_CACHE_TIMEOUT")
                        .map(|value| {
                            let value = value.to_str().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "invalid value for $BRIOCHE_CACHE_TIMEOUT: {}",
                                    value.display()
                                )
                            })?;
                            let duration = humantime::parse_duration(value)?;
                            anyhow::Ok(duration)
                        })
                        .transpose()?;
                    let connect_timeout = std::env::var_os("BRIOCHE_CACHE_CONNECT_TIMEOUT")
                        .map(|value| {
                            let value = value.to_str().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "invalid value for $BRIOCHE_CACHE_CONNECT_TIMEOUT: {}",
                                    value.display()
                                )
                            })?;
                            let duration = humantime::parse_duration(value)?;
                            anyhow::Ok(duration)
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

        Ok(Brioche {
            reporter,
            projects: Arc::new(RwLock::new(projects::Projects::default())),
            recipes: Arc::new(RwLock::new(recipe::Recipes::default())),
            data_dir,
            registry_client,
            cache_client,
            download_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
            download_client,
        })
    }
}
