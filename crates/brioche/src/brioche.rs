use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use futures::TryFutureExt as _;
use sha2::Digest as _;
use sqlx::Connection as _;
use tokio::sync::{Mutex, RwLock};

use crate::reporter::Reporter;

pub mod artifact;
pub mod blob;
pub mod input;
pub mod output;
pub mod platform;
pub mod project;
pub mod resolve;
pub mod script;

const MAX_CONCURRENT_PROCESSES: usize = 20;
const MAX_CONCURRENT_DOWNLOADS: usize = 20;

#[derive(Clone)]
pub struct Brioche {
    reporter: Reporter,
    db_conn: Arc<Mutex<sqlx::SqliteConnection>>,
    pub repo_dir: PathBuf,
    /// The directory where all of Brioche's data is stored. Usually configured
    /// to follow the platform's conventions for storing application data, such
    /// as `~/.local/share/brioche` on Linux.
    home: PathBuf,
    /// Causes Brioche to call itself to execute processes in a sandbox, rather
    /// than using a `tokio::spawn_blocking` thread. This could allow for
    /// running more processes at a time. This option mainly exists because
    /// it needs to be disabled when running tests.
    self_exec_processes: bool,
    /// Keep some temporary files that would otherwise be discarded. This is
    /// useful for debugging, where build outputs may succeed but need to be
    /// manually investigated.
    pub keep_temps: bool,
    pub proxies: Arc<RwLock<resolve::Proxies>>,
    pub active_resolves: Arc<RwLock<resolve::ActiveResolves>>,
    pub process_semaphore: Arc<tokio::sync::Semaphore>,
    pub download_semaphore: Arc<tokio::sync::Semaphore>,
    pub download_client: reqwest_middleware::ClientWithMiddleware,
}

pub struct BriocheBuilder {
    reporter: Reporter,
    home: Option<PathBuf>,
    repo_dir: Option<PathBuf>,
    self_exec_processes: bool,
    keep_temps: bool,
}

impl BriocheBuilder {
    pub fn new(reporter: Reporter) -> Self {
        Self {
            reporter,
            home: None,
            repo_dir: None,
            self_exec_processes: true,
            keep_temps: false,
        }
    }

    pub fn home(mut self, brioche_home: PathBuf) -> Self {
        self.home = Some(brioche_home);
        self
    }

    pub fn repo_dir(mut self, repo_dir: PathBuf) -> Self {
        self.repo_dir = Some(repo_dir);
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

    pub async fn build(self) -> anyhow::Result<Brioche> {
        let dirs = directories::ProjectDirs::from("dev", "brioche", "brioche")
            .context("failed to get Brioche directories (is $HOME set?)")?;
        let config_path = dirs.config_dir().join("config.toml");
        let config = tokio::fs::read_to_string(&config_path)
            .map_err(anyhow::Error::from)
            .and_then(|config| async move {
                let config = toml::from_str::<BriocheConfig>(&config)?;
                anyhow::Ok(config)
            })
            .map_err(|error| {
                error.context(format!(
                    "failed to read brioche config from {}",
                    config_path.display()
                ))
            })
            .await;

        let brioche_home = match self.home {
            Some(home) => home,
            None => dirs.data_local_dir().to_owned(),
        };

        let repo_dir = if let Some(repo) = self.repo_dir {
            repo
        } else if let Some(repo) = std::env::var_os("BRIOCHE_REPO") {
            PathBuf::from(repo)
        } else {
            let config = config?;
            config.repo_dir
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

        Ok(Brioche {
            reporter: self.reporter,
            db_conn: Arc::new(Mutex::new(db_conn)),
            home: brioche_home,
            repo_dir,
            self_exec_processes: self.self_exec_processes,
            keep_temps: self.keep_temps,
            proxies: Arc::new(RwLock::new(resolve::Proxies::default())),
            active_resolves: Arc::new(RwLock::new(resolve::ActiveResolves::default())),
            process_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PROCESSES)),
            download_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
            download_client,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BriocheConfig {
    repo_dir: PathBuf,
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
