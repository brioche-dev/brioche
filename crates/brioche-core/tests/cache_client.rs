use std::{collections::HashSet, sync::Arc};

use assert_matches::assert_matches;
use brioche_core::{
    blob::BlobHash,
    cache::CacheClient,
    recipe::{Artifact, Recipe},
    Brioche,
};
use futures::StreamExt as _;

#[tokio::test]
async fn test_cache_client_save_and_load_bake() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let recipe_hash;
    let artifact_hash;

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), true).await;

        let artifact = build_artifact(&brioche, 1024, &mut HashSet::new()).await;
        artifact_hash = artifact.hash();

        let recipe = brioche_test_support::default_process();
        let recipe = Recipe::Process(recipe);
        recipe_hash = recipe.hash();

        brioche_core::cache::save_bake(&brioche, recipe_hash, artifact_hash).await?;
    }

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), false).await;

        let loaded_bake = brioche_core::cache::load_bake(&brioche, recipe_hash).await?;

        assert_eq!(loaded_bake, Some(artifact_hash));
    }

    Ok(())
}

#[tokio::test]
async fn test_cache_client_save_and_load_artifact() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let mut blob_hashes = HashSet::new();
    let artifact_hash;
    let blob_size = 1024;

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), true).await;

        let artifact = build_artifact(&brioche, blob_size, &mut blob_hashes).await;
        artifact_hash = artifact.hash();

        brioche_core::cache::save_artifact(&brioche, artifact).await?;
    }

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), false).await;

        let loaded_artifact = brioche_core::cache::load_artifact(&brioche, artifact_hash).await?;

        let expected_artifact = build_artifact(&brioche, blob_size, &mut blob_hashes).await;
        assert_eq!(loaded_artifact, Some(expected_artifact));

        // Ensure all blobs have been saved locally
        for blob_hash in blob_hashes {
            let blob_path = brioche_core::blob::local_blob_path(&brioche, blob_hash);
            let blob_exists = tokio::fs::try_exists(&blob_path).await?;
            assert!(blob_exists);
        }
    }

    // The total size of the artifact should be small enough to be inlined
    // in the archive, so we shouldn't have any chunks
    let chunks = list_chunks(&cache).await?;
    assert!(chunks.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_cache_client_save_and_load_big_artifact() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let mut blob_hashes = HashSet::new();
    let artifact_hash;
    let blob_size = 10 * 1024 * 1024;

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), true).await;

        let artifact = build_artifact(&brioche, blob_size, &mut blob_hashes).await;
        artifact_hash = artifact.hash();

        brioche_core::cache::save_artifact(&brioche, artifact).await?;
    }

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), false).await;

        let loaded_artifact = brioche_core::cache::load_artifact(&brioche, artifact_hash).await?;

        let expected_artifact = build_artifact(&brioche, blob_size, &mut blob_hashes).await;
        assert_eq!(loaded_artifact, Some(expected_artifact));

        // Ensure all blobs have been saved locally
        for blob_hash in &blob_hashes {
            let blob_path = brioche_core::blob::local_blob_path(&brioche, *blob_hash);
            let blob_exists = tokio::fs::try_exists(&blob_path).await?;
            assert!(blob_exists);
        }
    }

    // The total size of the artifact should be too big to be inlined, so
    // we should have some chunks. Because each blob is so big, we should
    // have more chunks than blobs
    let chunks = list_chunks(&cache).await?;
    assert!(chunks.len() > blob_hashes.len());

    Ok(())
}

#[tokio::test]
async fn test_cache_client_load_artifact_hash_mismatch_error() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let mut blob_hashes = HashSet::new();
    let real_artifact_hash;
    let blob_size = 1024;

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), true).await;

        let real_artifact = build_artifact(&brioche, blob_size, &mut blob_hashes).await;
        real_artifact_hash = real_artifact.hash();

        let fake_artifact = brioche_test_support::dir_empty();
        let fake_artifact_hash = fake_artifact.hash();

        // Save the fake artifact
        brioche_core::cache::save_artifact(&brioche, fake_artifact.clone()).await?;

        // Move the fake artifact to where the real artifact should be
        cache
            .rename(
                &object_store::path::Path::parse(format!(
                    "artifacts/{fake_artifact_hash}.bar.zst"
                ))?,
                &object_store::path::Path::parse(format!(
                    "artifacts/{real_artifact_hash}.bar.zst"
                ))?,
            )
            .await?;
    }

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), false).await;

        // Try to load the real artifact (which should end up with the hash
        // of the fake artifact)
        let result = brioche_core::cache::load_artifact(&brioche, real_artifact_hash).await;
        assert_matches!(result, Err(_));
    }

    Ok(())
}

#[tokio::test]
async fn test_cache_client_load_artifact_missing_chunks_error() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let mut blob_hashes = HashSet::new();
    let artifact_hash;
    let blob_size = 10 * 1024 * 1024;

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), true).await;

        let artifact = build_artifact(&brioche, blob_size, &mut blob_hashes).await;
        artifact_hash = artifact.hash();

        brioche_core::cache::save_artifact(&brioche, artifact).await?;

        let chunks = list_chunks(&cache).await?;
        for chunk in chunks {
            cache.delete(&chunk).await?;
        }
    }

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), false).await;

        let loaded_artifact = brioche_core::cache::load_artifact(&brioche, artifact_hash).await;
        assert_matches!(loaded_artifact, Err(_));
    }

    Ok(())
}

#[tokio::test]
async fn test_cache_client_load_artifact_chunk_mismatch_error() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let mut blob_hashes = HashSet::new();
    let artifact_hash;
    let blob_size = 10 * 1024 * 1024;

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), true).await;

        let artifact = build_artifact(&brioche, blob_size, &mut blob_hashes).await;
        artifact_hash = artifact.hash();

        brioche_core::cache::save_artifact(&brioche, artifact).await?;

        let chunks = list_chunks(&cache).await?;
        for chunk in &chunks {
            eprintln!("Copying chunk {} to {chunk}", chunks[0]);
            cache.copy(&chunks[4], chunk).await?;
        }
    }

    let chunks = list_chunks(&cache).await?;
    for chunk in &chunks {
        let chunk_data = cache.get(chunk).await?.bytes().await?;
        let chunk_data = zstd::decode_all(chunk_data.as_ref())?;
        let chunk_data_hash = blake3::hash(&chunk_data);
        eprintln!("hash of {chunk}: {chunk_data_hash}");
    }

    {
        let (brioche, _) = brioche_test_with_cache(cache.clone(), false).await;

        let loaded_artifact = brioche_core::cache::load_artifact(&brioche, artifact_hash).await;
        assert_matches!(loaded_artifact, Err(_));
    }

    Ok(())
}

async fn brioche_test_with_cache(
    store: Arc<dyn object_store::ObjectStore>,
    writable: bool,
) -> (Brioche, brioche_test_support::TestContext) {
    brioche_test_support::brioche_test_with(move |builder| {
        builder.cache_client(CacheClient {
            store: Some(store),
            writable,
            ..Default::default()
        })
    })
    .await
}

async fn build_blob(brioche: &Brioche, data: impl AsRef<[u8]>, size: usize) -> BlobHash {
    let bytes = data
        .as_ref()
        .iter()
        .copied()
        .chain((0..=255).cycle())
        .take(size)
        .collect::<Vec<u8>>();
    brioche_test_support::blob(brioche, &bytes).await
}

async fn build_artifact(
    brioche: &Brioche,
    blob_size: usize,
    blob_hashes: &mut HashSet<BlobHash>,
) -> Artifact {
    let foo_bar_a_txt_blob = build_blob(brioche, "file a.txt", blob_size).await;
    blob_hashes.insert(foo_bar_a_txt_blob);

    let foo_bar_b_txt_blob = build_blob(brioche, "file b.txt", blob_size).await;
    blob_hashes.insert(foo_bar_b_txt_blob);

    let file_with_resources_txt_blob =
        build_blob(brioche, "file-with-resources.txt", blob_size).await;
    blob_hashes.insert(file_with_resources_txt_blob);

    let inner_resources_txt_blob = build_blob(brioche, "inner-resources.txt", blob_size).await;
    blob_hashes.insert(inner_resources_txt_blob);

    brioche_test_support::dir(
        brioche,
        [
            (
                "foo/bar/a.txt",
                brioche_test_support::file(foo_bar_a_txt_blob, false),
            ),
            (
                "foo/b.txt",
                brioche_test_support::file(foo_bar_b_txt_blob, true),
            ),
            ("a.txt", brioche_test_support::symlink("foo/a.txt")),
            (
                "file-with-resources.txt",
                brioche_test_support::file_with_resources(
                    file_with_resources_txt_blob,
                    true,
                    brioche_test_support::dir_value(
                        brioche,
                        [(
                            "inner-resources.txt",
                            brioche_test_support::file(inner_resources_txt_blob, false),
                        )],
                    )
                    .await,
                ),
            ),
        ],
    )
    .await
}

async fn list_chunks(
    cache: &dyn object_store::ObjectStore,
) -> anyhow::Result<Vec<object_store::path::Path>> {
    let chunks = cache
        .list(Some(&object_store::path::Path::parse("chunks")?))
        .collect::<Vec<_>>()
        .await;
    let chunks = chunks
        .into_iter()
        .map(|meta| Ok(meta?.location))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(chunks)
}
