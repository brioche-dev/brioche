use tokio::io::{AsyncReadExt as _, AsyncWriteExt};

use crate::{
    recipe::{Artifact, RecipeHash},
    Brioche,
};

mod archive;

pub async fn load_bake(
    brioche: &Brioche,
    input_hash: RecipeHash,
) -> anyhow::Result<Option<RecipeHash>> {
    let bake_path =
        object_store::path::Path::from_iter(["bakes", &input_hash.to_string(), "output_hash"]);
    let output_hash_object = brioche.cache_object_store.get(&bake_path).await;
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

pub async fn save_bake(
    brioche: &Brioche,
    input_hash: RecipeHash,
    output_hash: RecipeHash,
) -> anyhow::Result<bool> {
    let bake_path =
        object_store::path::Path::from_iter(["bakes", &input_hash.to_string(), "output_hash"]);

    let put_result = brioche
        .cache_object_store
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

pub async fn load_artifact(
    brioche: &Brioche,
    hash: RecipeHash,
) -> anyhow::Result<Option<Artifact>> {
    let artifact_filename = format!("{hash}.bar.zst");
    let artifact_path = object_store::path::Path::from_iter(["artifacts", &artifact_filename]);

    let archive_object = brioche.cache_object_store.get(&artifact_path).await;
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

    let artifact = archive::read_artifact_archive(brioche, &mut archive_reader).await?;

    let actual_hash = artifact.hash();
    anyhow::ensure!(
        actual_hash == hash,
        "artifact from cache at {artifact_path} has hash {actual_hash}, but expected {hash}"
    );

    Ok(Some(artifact))
}

pub async fn save_artifact(brioche: &Brioche, artifact: Artifact) -> anyhow::Result<bool> {
    let artifact_filename = format!("{}.bar.zst", artifact.hash());
    let artifact_path = object_store::path::Path::from_iter(["artifacts", &artifact_filename]);

    let mut archive_compressed = vec![];
    let mut archive_writer =
        async_compression::tokio::write::ZstdEncoder::new(&mut archive_compressed);
    archive::write_artifact_archive(brioche, artifact, &mut archive_writer).await?;
    archive_writer.shutdown().await?;

    let put_result = brioche
        .cache_object_store
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
