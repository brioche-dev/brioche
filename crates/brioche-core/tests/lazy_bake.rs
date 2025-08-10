use std::sync::Arc;

use brioche_core::{
    Brioche,
    cache::CacheClient,
    recipe::{DownloadRecipe, Recipe, Unarchive},
    registry::RegistryClient,
};

async fn is_lazy_bake_accepted(brioche: &Brioche, recipe: Recipe) -> bool {
    brioche_core::lazy_bake::is_cheap_to_bake_or_in_remote_cache(brioche, recipe.clone())
        .await
        .unwrap()
}

#[tokio::test]
async fn test_lazy_bake_always_accepts_cheap_recipes() -> anyhow::Result<()> {
    // Cheap resources are never cached, so we shouldn't even check the
    // cache for them. Using Mockito as the server lets us ensure that
    // the cache isn't even checked
    let mock_cache = mockito::Server::new_async().await;
    let (brioche, _context) = brioche_test_with_http_cache(&mock_cache.url()).await;

    // A literal file without any resources is cheap
    let hello_blob = brioche_test_support::blob(&brioche, "hello world").await;
    let hello_file = brioche_test_support::file(hello_blob, false);
    assert!(is_lazy_bake_accepted(&brioche, hello_file.clone().into()).await);

    // Unarchiving a file should be accepted if the archived file is accepted
    let unarchive_hello_file = Recipe::Unarchive(Unarchive {
        file: Box::new(brioche_test_support::without_meta(
            hello_file.clone().into(),
        )),
        archive: brioche_core::recipe::ArchiveFormat::Tar,
        compression: brioche_core::recipe::CompressionFormat::None,
    });
    assert!(is_lazy_bake_accepted(&brioche, unarchive_hello_file).await);

    // Directories are never baked and are always cheap
    let dir_with_hello = brioche_test_support::dir(&brioche, [("hello.txt", hello_file)]).await;
    assert!(is_lazy_bake_accepted(&brioche, dir_with_hello.clone().into()).await);

    Ok(())
}

#[tokio::test]
async fn test_lazy_bake_accepts_uncached_expensive_recipes_when_cached() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, _context) = brioche_test_with_cache(cache.clone(), true).await;

    let hello_blob = brioche_test_support::blob(&brioche, "hello world").await;
    let hello_file = brioche_test_support::file(hello_blob, false);

    // Process, download, and sync recipes are expensive, so they should
    // only be accepted when cached
    let process = Recipe::Process(brioche_test_support::default_process_x86_64_linux());
    let download = Recipe::Download(DownloadRecipe {
        url: "https://example.com".parse().unwrap(),
        hash: brioche_core::Hash::Sha256 { value: vec![0; 32] },
    });
    let sync_hello_file = Recipe::Sync {
        recipe: Box::new(brioche_test_support::without_meta(
            hello_file.clone().into(),
        )),
    };

    // A lazy dir should only be accepted if all of its entries are accepted
    let lazy_dir_with_expensive_recipes = brioche_test_support::lazy_dir([
        ("foo", process.clone()),
        ("bar", download.clone()),
        ("baz", sync_hello_file.clone()),
    ]);

    // With an empty cache, none of these recipes should be accepted
    assert!(!is_lazy_bake_accepted(&brioche, process.clone()).await);
    assert!(!is_lazy_bake_accepted(&brioche, download.clone()).await);
    assert!(!is_lazy_bake_accepted(&brioche, sync_hello_file.clone()).await);
    assert!(!is_lazy_bake_accepted(&brioche, lazy_dir_with_expensive_recipes.clone()).await);

    // Cache the result of the process recipe
    brioche_core::cache::save_bake(&brioche, process.hash(), hello_file.hash())
        .await
        .unwrap();

    // Just the process recipe should be accepted now
    assert!(is_lazy_bake_accepted(&brioche, process.clone()).await);
    assert!(!is_lazy_bake_accepted(&brioche, download.clone()).await);
    assert!(!is_lazy_bake_accepted(&brioche, sync_hello_file.clone()).await);
    assert!(!is_lazy_bake_accepted(&brioche, lazy_dir_with_expensive_recipes.clone()).await);

    // Save the download recipe
    brioche_core::cache::save_bake(&brioche, download.hash(), hello_file.hash())
        .await
        .unwrap();

    // Now the process and download recipes should be accepted
    assert!(is_lazy_bake_accepted(&brioche, process.clone()).await);
    assert!(is_lazy_bake_accepted(&brioche, download.clone()).await);
    assert!(!is_lazy_bake_accepted(&brioche, sync_hello_file.clone()).await);
    assert!(!is_lazy_bake_accepted(&brioche, lazy_dir_with_expensive_recipes.clone()).await);

    // Save the sync recipe
    brioche_core::cache::save_bake(&brioche, sync_hello_file.hash(), hello_file.hash())
        .await
        .unwrap();

    // Each recipe should be accepted now
    assert!(is_lazy_bake_accepted(&brioche, process.clone()).await);
    assert!(is_lazy_bake_accepted(&brioche, download.clone()).await);
    assert!(is_lazy_bake_accepted(&brioche, sync_hello_file.clone()).await);
    assert!(is_lazy_bake_accepted(&brioche, lazy_dir_with_expensive_recipes.clone()).await);

    Ok(())
}

async fn brioche_test_with_http_cache(url: &str) -> (Brioche, brioche_test_support::TestContext) {
    let store = object_store::http::HttpBuilder::new()
        .with_url(url)
        .with_retry(object_store::RetryConfig {
            max_retries: 0,
            ..Default::default()
        })
        .build()
        .unwrap();

    brioche_test_with_cache(Arc::new(store), false).await
}

async fn brioche_test_with_cache(
    store: Arc<dyn object_store::ObjectStore>,
    writable: bool,
) -> (Brioche, brioche_test_support::TestContext) {
    brioche_test_support::brioche_test_with(move |builder| {
        builder
            .cache_client(CacheClient {
                store: Some(store),
                writable,
                ..Default::default()
            })
            .registry_client(RegistryClient::disabled())
    })
    .await
}
