use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use config::BriocheConfig;
use registry::RegistryClient;
use reporter::Reporter;
use sandbox::SandboxBackend;
use sha2::Digest as _;
use tokio::sync::RwLock;
use tracing::Instrument as _;

pub mod bake;
pub mod blob;
pub mod cache;
pub mod config;
pub mod download;
pub mod encoding;
pub mod fs_utils;
pub mod input;
pub mod lazy_bake;
pub mod object_store_utils;
pub mod output;
pub mod platform;
pub mod process_events;
pub mod project;
pub mod publish;
pub mod recipe;
pub mod references;
pub mod registry;
pub mod reporter;
pub mod sandbox;
pub mod script;
pub mod sync;
pub mod utils;
pub mod vfs;

const MAX_CONCURRENT_PROCESSES: usize = 20;
const MAX_CONCURRENT_DOWNLOADS: usize = 20;
const DB_POOL_MIN_CONNECTIONS: u32 = 1;
const DB_POOL_MAX_CONNECTIONS: u32 = 10;

const DEFAULT_REGISTRY_URL: &str = "https://registry.brioche.dev/";
pub const USER_AGENT: &str = concat!("brioche/", env!("CARGO_PKG_VERSION"));
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct Brioche {
    reporter: Reporter,

    pub vfs: vfs::Vfs,

    db_pool: sqlx::Pool<sqlx::Sqlite>,

    /// The directory where all of Brioche's data is stored. Usually configured
    /// to follow the platform's conventions for storing application data, such
    /// as `~/.local/share/brioche` on Linux.
    pub data_dir: PathBuf,

    /// Causes Brioche to call itself to execute processes in a sandbox, rather
    /// than using a `tokio::spawn_blocking` thread. This could allow for
    /// running more processes at a time. This option mainly exists because
    /// it needs to be disabled when running tests.
    self_exec_processes: bool,

    /// Keep some temporary files that would otherwise be discarded. This is
    /// useful for debugging, where build outputs may succeed but need to be
    /// manually investigated.
    pub keep_temps: bool,

    /// Synchronize baked recipes to the registry automatically.
    pub sync_tx: Arc<tokio::sync::mpsc::Sender<SyncMessage>>,

    pub cached_recipes: Arc<RwLock<bake::CachedRecipes>>,

    pub active_bakes: Arc<RwLock<bake::ActiveBakes>>,

    pub process_semaphore: Arc<tokio::sync::Semaphore>,

    pub download_semaphore: Arc<tokio::sync::Semaphore>,

    pub download_client: reqwest_middleware::ClientWithMiddleware,

    pub registry_client: registry::RegistryClient,

    pub cache_client: cache::CacheClient,

    pub sandbox_config: config::SandboxConfig,

    sandbox_backend: Arc<tokio::sync::OnceCell<sandbox::SandboxBackend>>,

    cancellation_token: tokio_util::sync::CancellationToken,

    /// Track running tasks that need to finish before exiting Brioche, even
    /// on Ctrl-C. Each spawned task should be cancellable using the
    /// cancellation token
    task_tracker: tokio_util::task::TaskTracker,
}

impl Brioche {
    /// Tell all running tasks to cancel early. Returns immediately, use
    /// [`Self::wait_for_tasks`] to wait until all tasks have stopped.
    pub fn cancel_tasks(&self) {
        self.cancellation_token.cancel();
    }

    pub async fn wait_for_tasks(&self) {
        self.task_tracker.close();
        self.task_tracker.wait().await;
    }
}

pub struct BriocheBuilder {
    reporter: Reporter,
    registry_client: Option<registry::RegistryClient>,
    cache_client: Option<cache::CacheClient>,
    vfs: vfs::Vfs,
    config: Option<BriocheConfig>,
    data_dir: Option<PathBuf>,
    sandbox_backend: Option<sandbox::SandboxBackend>,
    self_exec_processes: bool,
    keep_temps: bool,
    sync: bool,
}

impl BriocheBuilder {
    #[must_use]
    pub fn new(reporter: Reporter) -> Self {
        Self {
            reporter,
            registry_client: None,
            cache_client: None,
            vfs: vfs::Vfs::immutable(),
            config: None,
            data_dir: None,
            sandbox_backend: None,
            self_exec_processes: true,
            keep_temps: false,
            sync: false,
        }
    }

    #[must_use]
    pub fn config(mut self, config: BriocheConfig) -> Self {
        self.config = Some(config);
        self
    }

    #[must_use]
    pub fn data_dir(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = Some(data_dir);
        self
    }

    #[must_use]
    pub fn registry_client(mut self, registry_client: RegistryClient) -> Self {
        self.registry_client = Some(registry_client);
        self
    }

    #[must_use]
    pub fn cache_client(mut self, cache_client: cache::CacheClient) -> Self {
        self.cache_client = Some(cache_client);
        self
    }

    #[must_use]
    pub fn sandbox_backend(mut self, sandbox_backend: SandboxBackend) -> Self {
        self.sandbox_backend = Some(sandbox_backend);
        self
    }

    #[must_use]
    pub const fn self_exec_processes(mut self, self_exec_processes: bool) -> Self {
        self.self_exec_processes = self_exec_processes;
        self
    }

    #[must_use]
    pub const fn keep_temps(mut self, keep_temps: bool) -> Self {
        self.keep_temps = keep_temps;
        self
    }

    #[must_use]
    pub fn vfs(mut self, vfs: vfs::Vfs) -> Self {
        self.vfs = vfs;
        self
    }

    #[must_use]
    pub const fn sync(mut self, sync: bool) -> Self {
        self.sync = sync;
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

        let data_dir = match (self.data_dir, std::env::var_os("BRIOCHE_DATA_DIR")) {
            (Some(data_dir), _) => data_dir,
            (None, Some(data_dir)) => PathBuf::from(data_dir),
            (None, None) => dirs.data_local_dir().to_owned(),
        };
        tokio::fs::create_dir_all(&data_dir).await?;

        let database_path = data_dir.join("brioche.db");

        let db_pool_options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .auto_vacuum(sqlx::sqlite::SqliteAutoVacuum::Full);
        let db_pool = sqlx::sqlite::SqlitePoolOptions::new()
            .min_connections(DB_POOL_MIN_CONNECTIONS)
            .max_connections(DB_POOL_MAX_CONNECTIONS)
            .connect_with(db_pool_options)
            .await?;

        tracing::debug!(
            database_path = %database_path.display(),
            "connected to database"
        );

        Self::sqlx_run_migrations(&db_pool).await?;

        tracing::debug!("finished running database migrations");

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
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .pool_max_idle_per_host(10)
            .build()?;
        let download_client = reqwest_middleware::ClientBuilder::new(download_client)
            .with(download_retry_middleware)
            .build();

        let registry_client = self.registry_client.unwrap_or_else(|| {
            let registry_password = std::env::var("BRIOCHE_REGISTRY_PASSWORD").ok();
            let registry_auth = registry_password
                .map_or(registry::RegistryAuthentication::Anonymous, |password| {
                    registry::RegistryAuthentication::Admin { password }
                });
            let registry_url = config.registry_url.clone().unwrap_or_else(|| {
                DEFAULT_REGISTRY_URL
                    .parse()
                    .expect("failed to parse default registry URL")
            });
            registry::RegistryClient::new(registry_url, registry_auth)
        });
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

        let (sync_tx, mut sync_rx) = tokio::sync::mpsc::channel(1000);

        // Start a task that listens for sync messages and syncs to the
        // registry during builds. This allows for some bakes to be synced
        // even if the overall build fails.
        let sync_enabled = self.sync;
        tokio::spawn(
            async move {
                let mut sync_results = sync::SyncBakesResults::default();

                while let Some(sync_message) = sync_rx.recv().await {
                    match sync_message {
                        SyncMessage::StartSync {
                            brioche,
                            recipe,
                            artifact,
                        } => {
                            if sync_enabled {
                                let result =
                                    sync::sync_bakes(&brioche, vec![(recipe, artifact)], false)
                                        .await
                                        .inspect_err(|error| {
                                            tracing::warn!("failed to sync baked recipe: {error}");
                                        });
                                if let Ok(result) = result {
                                    sync_results.merge(result);
                                }
                            }
                        }
                        SyncMessage::Flush { completed } => {
                            let results = std::mem::take(&mut sync_results);
                            let _ = completed.send(results).inspect_err(|_| {
                                tracing::warn!("failed to send sync flush completion");
                            });
                        }
                    }
                }
            }
            .instrument(tracing::Span::current()),
        );

        let cancellation_token = tokio_util::sync::CancellationToken::new();
        let task_tracker = tokio_util::task::TaskTracker::new();

        Ok(Brioche {
            reporter: self.reporter,
            vfs: self.vfs,
            db_pool,
            data_dir,
            self_exec_processes: self.self_exec_processes,
            keep_temps: self.keep_temps,
            sync_tx: Arc::new(sync_tx),
            cached_recipes: Arc::new(RwLock::new(bake::CachedRecipes::default())),
            active_bakes: Arc::new(RwLock::new(bake::ActiveBakes::default())),
            process_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PROCESSES)),
            download_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
            download_client,
            registry_client,
            cache_client,
            sandbox_config: config.sandbox.clone(),
            sandbox_backend: Arc::new(tokio::sync::OnceCell::new_with(self.sandbox_backend)),
            cancellation_token,
            task_tracker,
        })
    }

    async fn sqlx_run_migrations(db_pool: &sqlx::Pool<sqlx::Sqlite>) -> anyhow::Result<()> {
        sqlx::migrate!().run(db_pool).await?;

        // WAL checkpoint to ensure migrations are fully committed
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(db_pool)
            .await?;

        Ok(())
    }
}

#[expect(clippy::large_enum_variant)]
pub enum SyncMessage {
    StartSync {
        brioche: Brioche,
        recipe: recipe::Recipe,
        artifact: recipe::Artifact,
    },
    Flush {
        completed: tokio::sync::oneshot::Sender<sync::SyncBakesResults>,
    },
}

#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/runtime"]
#[include = "dist/**/*.js"]
#[include = "tslib/**/*.d.ts"]
pub struct RuntimeFiles;

#[serde_with::serde_as]
#[derive(Debug, Clone, Hash, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum Hash {
    Sha256 {
        #[serde_as(as = "serde_with::hex::Hex")]
        value: Vec<u8>,
    },
}

impl std::fmt::Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sha256 { value } => write!(f, "sha256:{}", hex::encode(value)),
        }
    }
}

pub enum Hasher {
    Sha256(sha2::Sha256),
}

impl Hasher {
    #[must_use]
    pub fn new_sha256() -> Self {
        Self::Sha256(sha2::Sha256::new())
    }

    #[must_use]
    pub fn for_hash(hash: &Hash) -> Self {
        match hash {
            Hash::Sha256 { .. } => Self::Sha256(sha2::Sha256::new()),
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(bytes),
        }
    }

    pub fn finish(self) -> anyhow::Result<Hash> {
        match self {
            Self::Sha256(hasher) => {
                let hash = hasher.finalize();
                Ok(Hash::Sha256 {
                    value: hash.to_vec(),
                })
            }
        }
    }
}
