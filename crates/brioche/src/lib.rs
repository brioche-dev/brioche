use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use registry::RegistryClient;
use reporter::Reporter;
use sha2::Digest as _;
use sqlx::Connection as _;
use tokio::{
    io::AsyncReadExt as _,
    sync::{Mutex, RwLock},
};

pub mod bake;
pub mod blob;
pub mod encoding;
pub mod fs_utils;
pub mod input;
pub mod output;
pub mod platform;
pub mod project;
pub mod recipe;
pub mod references;
pub mod registry;
pub mod reporter;
pub mod sandbox;
pub mod script;
pub mod sync;
pub mod vfs;

const MAX_CONCURRENT_PROCESSES: usize = 20;
const MAX_CONCURRENT_DOWNLOADS: usize = 20;

const DEFAULT_REGISTRY_URL: &str = "https://registry.brioche.dev/";

#[derive(Clone)]
pub struct Brioche {
    reporter: Reporter,
    pub vfs: vfs::Vfs,
    db_conn: Arc<Mutex<sqlx::SqliteConnection>>,
    /// The directory where all of Brioche's data is stored. Usually configured
    /// to follow the platform's conventions for storing application data, such
    /// as `~/.local/share/brioche` on Linux.
    pub home: PathBuf,
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
}

pub struct BriocheBuilder {
    reporter: Reporter,
    registry_client: Option<registry::RegistryClient>,
    vfs: vfs::Vfs,
    home: Option<PathBuf>,
    self_exec_processes: bool,
    keep_temps: bool,
    sync: bool,
}

impl BriocheBuilder {
    pub fn new(reporter: Reporter) -> Self {
        Self {
            reporter,
            registry_client: None,
            vfs: vfs::Vfs::immutable(),
            home: None,
            self_exec_processes: true,
            keep_temps: false,
            sync: false,
        }
    }

    pub fn home(mut self, brioche_home: PathBuf) -> Self {
        self.home = Some(brioche_home);
        self
    }

    pub fn registry_client(mut self, registry_client: RegistryClient) -> Self {
        self.registry_client = Some(registry_client);
        self
    }

    pub fn self_exec_processes(mut self, self_exec_processes: bool) -> Self {
        self.self_exec_processes = self_exec_processes;
        self
    }

    pub fn keep_temps(mut self, keep_temps: bool) -> Self {
        self.keep_temps = keep_temps;
        self
    }

    pub fn vfs(mut self, vfs: vfs::Vfs) -> Self {
        self.vfs = vfs;
        self
    }

    pub fn sync(mut self, sync: bool) -> Self {
        self.sync = sync;
        self
    }

    pub async fn build(self) -> anyhow::Result<Brioche> {
        let dirs = directories::ProjectDirs::from("dev", "brioche", "brioche")
            .context("failed to get Brioche directories (is $HOME set?)")?;
        let config_path = dirs.config_dir().join("config.toml");
        let config_file = match tokio::fs::File::open(&config_path).await {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    "config file not found at {}, using default configuration",
                    config_path.display()
                );
                None
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to open brioche config file at {}",
                        config_path.display()
                    )
                });
            }
        };
        let config = match config_file {
            Some(mut file) => {
                let mut config = String::new();
                file.read_to_string(&mut config).await.with_context(|| {
                    format!(
                        "failed to read brioche config from {}",
                        config_path.display()
                    )
                })?;
                toml::from_str::<BriocheConfig>(&config).with_context(|| {
                    format!(
                        "failed to parse brioche config from {}",
                        config_path.display()
                    )
                })?
            }
            None => BriocheConfig::default(),
        };

        let brioche_home = match self.home {
            Some(home) => home,
            None => dirs.data_local_dir().to_owned(),
        };

        tokio::fs::create_dir_all(&brioche_home).await?;

        let database_path = brioche_home.join("brioche.db");

        let db_conn_options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .auto_vacuum(sqlx::sqlite::SqliteAutoVacuum::Full);
        let mut db_conn = sqlx::sqlite::SqliteConnection::connect_with(&db_conn_options).await?;

        tracing::debug!(
            database_path = %database_path.display(),
            "connected to database"
        );

        sqlx::migrate!().run(&mut db_conn).await?;

        tracing::debug!("finished running database migrations");

        let download_retry_policy =
            reqwest_retry::policies::ExponentialBackoff::builder().build_with_max_retries(5);
        let download_retry_middleware =
            reqwest_retry::RetryTransientMiddleware::new_with_policy(download_retry_policy);
        let download_client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
            .with(download_retry_middleware)
            .build();

        let registry_client = self.registry_client.unwrap_or_else(|| {
            let registry_password = std::env::var("BRIOCHE_REGISTRY_PASSWORD").ok();
            let registry_auth = match registry_password {
                Some(password) => registry::RegistryAuthentication::Admin { password },
                None => registry::RegistryAuthentication::Anonymous,
            };
            let registry_url = config.registry_url.clone().unwrap_or_else(|| {
                DEFAULT_REGISTRY_URL
                    .parse()
                    .expect("failed to parse default registry URL")
            });
            registry::RegistryClient::new(registry_url, registry_auth)
        });

        let (sync_tx, mut sync_rx) = tokio::sync::mpsc::channel(1000);

        // Start a task that listens for sync messages and syncs to the
        // registry during builds. This allows for some bakes to be synced
        // even if the overall build fails.
        let sync_enabled = self.sync;
        tokio::spawn(async move {
            let mut sync_results = sync::SyncResults::default();

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
        });

        Ok(Brioche {
            reporter: self.reporter,
            vfs: self.vfs,
            db_conn: Arc::new(Mutex::new(db_conn)),
            home: brioche_home,
            self_exec_processes: self.self_exec_processes,
            keep_temps: self.keep_temps,
            sync_tx: Arc::new(sync_tx),
            cached_recipes: Arc::new(RwLock::new(bake::CachedRecipes::default())),
            active_bakes: Arc::new(RwLock::new(bake::ActiveBakes::default())),
            process_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PROCESSES)),
            download_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
            download_client,
            registry_client,
        })
    }
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct BriocheConfig {
    registry_url: Option<url::Url>,
}

pub enum SyncMessage {
    StartSync {
        brioche: Brioche,
        recipe: recipe::Recipe,
        artifact: recipe::Artifact,
    },
    Flush {
        completed: tokio::sync::oneshot::Sender<sync::SyncResults>,
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
            Hash::Sha256 { value } => write!(f, "sha256:{}", hex::encode(value)),
        }
    }
}

pub enum Hasher {
    Sha256(sha2::Sha256),
}

impl Hasher {
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
                    value: hash.as_slice().to_vec(),
                })
            }
        }
    }
}
