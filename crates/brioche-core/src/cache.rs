use std::sync::Arc;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt};

use crate::{
    project::ProjectHash,
    recipe::{Artifact, RecipeHash},
    reporter::job::CacheFetchKind,
    Brioche,
};

mod archive;

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

#[tracing::instrument(skip(brioche))]
pub async fn load_bake(
    brioche: &Brioche,
    input_hash: RecipeHash,
) -> anyhow::Result<Option<RecipeHash>> {
    let Some(store) = brioche.cache_client.store.clone() else {
        return Ok(None);
    };

    let bake_path =
        object_store::path::Path::from_iter(["bakes", &input_hash.to_string(), "output_hash"]);
    let output_hash_object = store.get(&bake_path).await;
    let output_hash_object = match output_hash_object {
        Ok(output_hash_object) => output_hash_object,
        Err(object_store::Error::NotFound { .. }) => return Ok(None),
        Err(error) => {
            return Err(error.into());
        }
    };

    let output_hash_stream = output_hash_object.into_stream();
    let output_hash_reader = tokio_util::io::StreamReader::new(output_hash_stream);

    // Read the output hash into a string. Since we know the output hash is
    // a 64-character hexadecimal string, we limit the reader to prevent
    // reading an arbitrarily large object into memory. We read up to 65
    // characters to detect malformed objects.
    let mut output_hash = String::new();
    output_hash_reader
        .take(65)
        .read_to_string(&mut output_hash)
        .await?;

    let output_hash: RecipeHash = output_hash.parse()?;
    Ok(Some(output_hash))
}

#[tracing::instrument(skip(brioche))]
pub async fn save_bake(
    brioche: &Brioche,
    input_hash: RecipeHash,
    output_hash: RecipeHash,
) -> anyhow::Result<bool> {
    let store = brioche.cache_client.writable_store()?;

    let bake_path =
        object_store::path::Path::from_iter(["bakes", &input_hash.to_string(), "output_hash"]);

    let put_result = store
        .put_opts(
            &bake_path,
            output_hash.to_string().into(),
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

    let project_artifact_hash_path = object_store::path::Path::from_iter([
        "projects",
        &project_hash.to_string(),
        "artifact_hash",
    ]);
    let project_artifact_hash_object = store.get(&project_artifact_hash_path).await;
    let project_artifact_hash_object = match project_artifact_hash_object {
        Ok(object) => object,
        Err(object_store::Error::NotFound { .. }) => return Ok(None),
        Err(error) => {
            return Err(error.into());
        }
    };

    let project_artifact_hash_stream = project_artifact_hash_object.into_stream();
    let project_artifact_hash_reader =
        tokio_util::io::StreamReader::new(project_artifact_hash_stream);

    // Read the artifact hash into a string. Since we know the artifact hash is
    // a 64-character hexadecimal string, we limit the reader to prevent
    // reading an arbitrarily large object into memory. We read up to 65
    // characters to detect malformed objects.
    let mut project_artifact_hash = String::new();
    project_artifact_hash_reader
        .take(65)
        .read_to_string(&mut project_artifact_hash)
        .await?;

    let project_artifact_hash: RecipeHash = project_artifact_hash.parse()?;
    Ok(Some(project_artifact_hash))
}

#[tracing::instrument(skip(brioche))]
pub async fn save_project_artifact_hash(
    brioche: &Brioche,
    project_hash: ProjectHash,
    artifact_hash: RecipeHash,
) -> anyhow::Result<bool> {
    let store = brioche.cache_client.writable_store()?;

    let project_artifact_hash_path = object_store::path::Path::from_iter([
        "projects",
        &project_hash.to_string(),
        "artifact_hash",
    ]);

    let put_result = store
        .put_opts(
            &project_artifact_hash_path,
            artifact_hash.to_string().into(),
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
