use std::sync::Arc;

use anyhow::Context as _;
use tokio::io::AsyncWriteExt as _;

use crate::{
    Brioche,
    project::ProjectHash,
    recipe::{Artifact, RecipeHash},
    reporter::job::CacheFetchKind,
};

mod archive;

pub const DEFAULT_CACHE_URL: &str = "https://cache.brioche.dev/";
pub const DEFAULT_CACHE_MAX_CONCURRENT_OPERATIONS: usize = 200;

#[derive(Debug, Default, Clone)]
pub struct CacheClient {
    pub store: Option<Arc<dyn object_store::ObjectStore>>,
    pub writable: bool,
    pub max_concurrent_chunk_fetches: Option<usize>,
}

impl CacheClient {
    fn store(&self) -> Option<Arc<dyn object_store::ObjectStore>> {
        self.store.clone()
    }

    fn writable_store(&self) -> anyhow::Result<Arc<dyn object_store::ObjectStore>> {
        let Some(store) = self.try_writable_store() else {
            anyhow::bail!("tried to write to cache, but no writable cache is configured");
        };

        Ok(store)
    }

    fn try_writable_store(&self) -> Option<Arc<dyn object_store::ObjectStore>> {
        if !self.writable {
            return None;
        }

        self.store()
    }
}

pub async fn cache_client_from_config_or_default(
    config: Option<&crate::config::CacheConfig>,
) -> anyhow::Result<CacheClient> {
    let max_concurrent_operations = config
        .map_or(DEFAULT_CACHE_MAX_CONCURRENT_OPERATIONS, |config| {
            config.max_concurrent_operations
        });
    let (url, writable) = match config {
        Some(config) => (config.url.clone(), !config.read_only),
        None => (DEFAULT_CACHE_URL.parse()?, false),
    };

    let retry_config = object_store::RetryConfig {
        backoff: object_store::BackoffConfig {
            init_backoff: std::time::Duration::from_secs(1),
            max_backoff: std::time::Duration::from_secs(30),
            ..Default::default()
        },
        max_retries: 5,
        ..Default::default()
    };

    let mut client_options = object_store::ClientOptions::new()
        .with_user_agent(http::HeaderValue::from_static(crate::USER_AGENT));
    if let Some(allow_http) = config.and_then(|config| config.allow_http) {
        client_options = client_options.with_allow_http(allow_http);
    }

    let store: Arc<dyn object_store::ObjectStore> = match url.scheme() {
        "http" | "https" => {
            let store = object_store::http::HttpBuilder::new()
                .with_url(url)
                .with_retry(retry_config)
                .with_client_options(client_options)
                .build()?;
            let store = object_store::limit::LimitStore::new(store, max_concurrent_operations);
            Arc::new(store)
        }
        "s3" => {
            let bucket = url
                .host_str()
                .context("S3 cache URL must include a bucket name")?;
            let prefix = url.path().trim_start_matches('/').to_string();

            let aws_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;

            let store_credentials =
                crate::object_store_utils::AwsS3CredentialProvider::new(aws_config.clone());
            let store_config = crate::object_store_utils::load_s3_config(&aws_config);

            let store = crate::object_store_utils::apply_s3_config(
                store_config,
                object_store::aws::AmazonS3Builder::new(),
            )
            .with_credentials(Arc::new(store_credentials))
            .with_bucket_name(bucket)
            .with_conditional_put(object_store::aws::S3ConditionalPut::ETagMatch)
            .with_client_options(client_options)
            .with_retry(retry_config)
            .build()?;
            let store = object_store::prefix::PrefixStore::new(store, prefix);

            let store = object_store::limit::LimitStore::new(store, max_concurrent_operations);
            Arc::new(store)
        }
        "file" => {
            let path = url
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("invalid file:// URL for cache"))?;

            let store = object_store::local::LocalFileSystem::new_with_prefix(&path)
                .with_context(|| format!("failed to use path {path:?} as cache"))?;
            let store = store.with_automatic_cleanup(true);
            let store = object_store::limit::LimitStore::new(store, max_concurrent_operations);
            Arc::new(store)
        }
        "memory" => {
            let store = object_store::memory::InMemory::new();
            let store = object_store::limit::LimitStore::new(store, max_concurrent_operations);
            Arc::new(store)
        }
        scheme => {
            anyhow::bail!("unknown scheme for cache: {scheme:?}");
        }
    };

    Ok(CacheClient {
        store: Some(store),
        writable,
        max_concurrent_chunk_fetches: Some(DEFAULT_CACHE_MAX_CONCURRENT_OPERATIONS),
    })
}

#[tracing::instrument(skip(brioche))]
pub async fn load_bake(
    brioche: &Brioche,
    input_hash: RecipeHash,
) -> anyhow::Result<Option<RecipeHash>> {
    let Some(store) = brioche.cache_client.store.clone() else {
        return Ok(None);
    };

    let bake_output_path =
        object_store::path::Path::from_iter(["bakes", &input_hash.to_string(), "output.json"]);
    let bake_output_object = store.get(&bake_output_path).await;
    let bake_output_object = match bake_output_object {
        Ok(bake_output_object) => bake_output_object,
        Err(object_store::Error::NotFound { .. }) => return Ok(None),
        Err(error) => {
            return Err(error.into());
        }
    };

    let bake_output_json = bake_output_object.bytes().await?;
    let bake_output: CachedBakeOutput = serde_json::from_slice(&bake_output_json[..])
        .with_context(|| format!("failed to deserialize cache object at '{bake_output_path}'"))?;

    Ok(Some(bake_output.output_hash))
}

#[tracing::instrument(skip(brioche))]
pub async fn save_bake(
    brioche: &Brioche,
    input_hash: RecipeHash,
    output_hash: RecipeHash,
) -> anyhow::Result<bool> {
    let store = brioche.cache_client.writable_store()?;

    let bake_output_path =
        object_store::path::Path::from_iter(["bakes", &input_hash.to_string(), "output.json"]);
    let bake_output = CachedBakeOutput { output_hash };
    let bake_output_json = serde_json::to_string(&bake_output)?;

    let put_result = store
        .put_opts(
            &bake_output_path,
            bake_output_json.into(),
            object_store::PutOptions {
                mode: object_store::PutMode::Create,
                ..Default::default()
            },
        )
        .await;

    let did_create = match put_result {
        Ok(_) => true,
        Err(object_store::Error::AlreadyExists { .. }) => false,
        Err(error) => {
            return Err(error.into());
        }
    };

    Ok(did_create)
}

#[tracing::instrument(skip(brioche))]
pub async fn load_artifact(
    brioche: &Brioche,
    artifact_hash: RecipeHash,
    fetch_kind: CacheFetchKind,
) -> anyhow::Result<Option<Artifact>> {
    let Some(store) = brioche.cache_client.store.clone() else {
        return Ok(None);
    };

    let artifact_filename = format!("{artifact_hash}.bar.zst");
    let artifact_path = object_store::path::Path::from_iter(["artifacts", &artifact_filename]);

    let archive_object = store.get(&artifact_path).await;
    let archive_object = match archive_object {
        Ok(archive_object) => archive_object,
        Err(object_store::Error::NotFound { .. }) => return Ok(None),
        Err(error) => {
            return Err(error.into());
        }
    };

    let archive_stream_compressed = archive_object.into_stream();
    let archive_reader_compressed = tokio_util::io::StreamReader::new(archive_stream_compressed);
    let mut archive_reader =
        async_compression::tokio::bufread::ZstdDecoder::new(archive_reader_compressed);

    let artifact =
        archive::read_artifact_archive(brioche, &store, fetch_kind, &mut archive_reader).await?;

    let actual_hash = artifact.hash();
    anyhow::ensure!(
        actual_hash == artifact_hash,
        "artifact from cache at {artifact_path} has hash {actual_hash}, but expected {artifact_hash}"
    );

    Ok(Some(artifact))
}

#[tracing::instrument(skip_all, fields(artifact_hash = %artifact.hash()))]
pub async fn save_artifact(brioche: &Brioche, artifact: Artifact) -> anyhow::Result<bool> {
    let store = brioche.cache_client.writable_store()?;

    let artifact_filename = format!("{}.bar.zst", artifact.hash());
    let artifact_path = object_store::path::Path::from_iter(["artifacts", &artifact_filename]);

    // Check if the artifact already exists in the cache. If it does, we
    // can return early. Note that another process or machine may still
    // end up writing the artifact before we do, but this check helps us
    // avoid doing extra work.
    let existing_object = store.head(&artifact_path).await;
    match existing_object {
        Ok(_) => {
            // The artifact already exists in the cache
            return Ok(false);
        }
        Err(object_store::Error::NotFound { .. }) => {
            // The artifact doesn't exist, so we can create it
        }
        Err(error) => {
            return Err(error.into());
        }
    }

    let mut archive_compressed = vec![];
    let mut archive_writer =
        async_compression::tokio::write::ZstdEncoder::new(&mut archive_compressed);
    archive::write_artifact_archive(brioche, artifact, &store, &mut archive_writer).await?;
    archive_writer.shutdown().await?;

    let put_result = store
        .put_opts(
            &artifact_path,
            archive_compressed.into(),
            object_store::PutOptions {
                mode: object_store::PutMode::Create,
                ..Default::default()
            },
        )
        .await;

    let did_create = match put_result {
        Ok(_) => true,
        Err(object_store::Error::AlreadyExists { .. }) => false,
        Err(error) => {
            return Err(error.into());
        }
    };

    Ok(did_create)
}

#[tracing::instrument(skip(brioche))]
pub async fn load_project_artifact_hash(
    brioche: &Brioche,
    project_hash: ProjectHash,
) -> anyhow::Result<Option<RecipeHash>> {
    let Some(store) = brioche.cache_client.store.clone() else {
        return Ok(None);
    };

    let project_source_path =
        object_store::path::Path::from_iter(["projects", &project_hash.to_string(), "source.json"]);
    let project_source_object = store.get(&project_source_path).await;
    let project_source_object = match project_source_object {
        Ok(project_source_object) => project_source_object,
        Err(object_store::Error::NotFound { .. }) => return Ok(None),
        Err(error) => {
            return Err(error.into());
        }
    };

    let project_source_json = project_source_object.bytes().await?;
    let project_source: CachedProjectSource = serde_json::from_slice(&project_source_json[..])
        .with_context(|| {
            format!("failed to deserialize cache object at '{project_source_path}'")
        })?;

    Ok(Some(project_source.artifact_hash))
}

#[tracing::instrument(skip(brioche))]
pub async fn save_project_artifact_hash(
    brioche: &Brioche,
    project_hash: ProjectHash,
    artifact_hash: RecipeHash,
) -> anyhow::Result<bool> {
    let store = brioche.cache_client.writable_store()?;

    let project_source_path =
        object_store::path::Path::from_iter(["projects", &project_hash.to_string(), "source.json"]);
    let project_source = CachedProjectSource { artifact_hash };
    let project_source_json = serde_json::to_string(&project_source)?;

    let put_result = store
        .put_opts(
            &project_source_path,
            project_source_json.into(),
            object_store::PutOptions {
                mode: object_store::PutMode::Create,
                ..Default::default()
            },
        )
        .await;

    let did_create = match put_result {
        Ok(_) => true,
        Err(object_store::Error::AlreadyExists { .. }) => false,
        Err(error) => {
            return Err(error.into());
        }
    };

    Ok(did_create)
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBakeOutput {
    output_hash: RecipeHash,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedProjectSource {
    artifact_hash: RecipeHash,
}
