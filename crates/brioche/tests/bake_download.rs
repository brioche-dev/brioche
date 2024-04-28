use assert_matches::assert_matches;
use brioche::recipe::{DownloadRecipe, Recipe};
use brioche_test::bake_without_meta;

mod brioche_test;

#[tokio::test]
async fn test_bake_download() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;
    let hello_hash = brioche_test::sha256(hello);
    let hello_endpoint = server
        .mock("GET", "/file.txt")
        .with_body(hello)
        .expect(1)
        .create();

    let hello_download = Recipe::Download(DownloadRecipe {
        hash: hello_hash,
        url: format!("{server_url}/file.txt").parse().unwrap(),
    });

    assert_eq!(
        bake_without_meta(&brioche, hello_download).await?,
        brioche_test::file(hello_blob, false),
    );

    hello_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_download_cached() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;
    let hello_hash = brioche_test::sha256(hello);
    let hello_endpoint = server
        .mock("GET", "/file.txt")
        .with_body(hello)
        .expect(1)
        .create();

    let hello_download = Recipe::Download(DownloadRecipe {
        hash: hello_hash.clone(),
        url: format!("{server_url}/file.txt").parse().unwrap(),
    });

    assert_eq!(
        bake_without_meta(&brioche, hello_download.clone()).await?,
        brioche_test::file(hello_blob, false),
    );

    assert_eq!(
        bake_without_meta(&brioche, hello_download).await?,
        brioche_test::file(hello_blob, false),
    );

    hello_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_download_rerun_after_failure() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;
    let hello_hash = brioche_test::sha256(hello);
    let hello_endpoint = server.mock("GET", "/file.txt").with_status(500).create();

    let hello_download = Recipe::Download(DownloadRecipe {
        hash: hello_hash.clone(),
        url: format!("{server_url}/file.txt").parse().unwrap(),
    });

    assert_matches!(
        bake_without_meta(&brioche, hello_download.clone()).await,
        Err(_)
    );

    hello_endpoint.assert();

    let hello_endpoint = hello_endpoint.with_status(200).with_body(hello).create();

    assert_eq!(
        bake_without_meta(&brioche, hello_download).await?,
        brioche_test::file(hello_blob, false),
    );

    hello_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_download_different_urls_with_same_hash() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;
    let hello_hash = brioche_test::sha256(hello);
    let file_1_endpoint = server.mock("GET", "/file1.txt").with_body(hello).create();
    let file_2_endpoint = server.mock("GET", "/file2.txt").with_body(hello).create();

    let file_1_download = Recipe::Download(DownloadRecipe {
        hash: hello_hash.clone(),
        url: format!("{server_url}/file1.txt").parse().unwrap(),
    });

    assert_eq!(
        bake_without_meta(&brioche, file_1_download).await?,
        brioche_test::file(hello_blob, false),
    );

    let file_2_download = Recipe::Download(DownloadRecipe {
        hash: hello_hash.clone(),
        url: format!("{server_url}/file2.txt").parse().unwrap(),
    });

    assert_eq!(
        bake_without_meta(&brioche, file_2_download).await?,
        brioche_test::file(hello_blob, false),
    );

    file_1_endpoint.assert();
    file_2_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_download_url_changed_hash() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;
    let hello_hash = brioche_test::sha256(hello);

    let hi = "hi";
    let hi_blob = brioche_test::blob(&brioche, hi).await;
    let hi_hash = brioche_test::sha256(hi);

    let file_endpoint = server.mock("GET", "/file.txt").with_body(hello).create();

    let file_download_1 = Recipe::Download(DownloadRecipe {
        hash: hello_hash.clone(),
        url: format!("{server_url}/file.txt").parse().unwrap(),
    });

    assert_eq!(
        bake_without_meta(&brioche, file_download_1).await?,
        brioche_test::file(hello_blob, false),
    );

    file_endpoint.assert();

    let file_endpoint = file_endpoint.with_body(hi).create();

    let file_download_2 = Recipe::Download(DownloadRecipe {
        hash: hi_hash.clone(),
        url: format!("{server_url}/file.txt").parse().unwrap(),
    });

    assert_eq!(
        bake_without_meta(&brioche, file_download_2).await?,
        brioche_test::file(hi_blob, false),
    );

    file_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_download_invalid_hash() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;
    let hello_hash = brioche_test::sha256(hello);
    let hello_endpoint = server
        .mock("GET", "/file.txt")
        .with_body("not hello")
        .create();

    let hello_download = Recipe::Download(DownloadRecipe {
        hash: hello_hash.clone(),
        url: format!("{server_url}/file.txt").parse().unwrap(),
    });

    assert_matches!(
        bake_without_meta(&brioche, hello_download.clone()).await,
        Err(_)
    );

    hello_endpoint.with_body(hello).create();

    assert_eq!(
        bake_without_meta(&brioche, hello_download).await?,
        brioche_test::file(hello_blob, false),
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_download_does_not_cache_using_only_hash() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;
    let hello_hash = brioche_test::sha256(hello);
    let hello_endpoint = server.mock("GET", "/hello.txt").with_body(hello).create();

    let hi = "hi";
    let hi_blob = brioche_test::blob(&brioche, hi).await;
    let hi_hash = brioche_test::sha256(hi);
    let hi_endpoint = server.mock("GET", "/hi.txt").with_body(hi).create();

    let hello_download = Recipe::Download(DownloadRecipe {
        hash: hello_hash.clone(),
        url: format!("{server_url}/hello.txt").parse().unwrap(),
    });

    // This download has the same hash as a previous download, but a different
    // URL. This should fail because we shouldn't cache by hash across URLs.
    // (This has always felt like a footgun in Nix because it's easy to update
    // the URL and to forget to update the hash, which will often do the
    // wrong thing)
    let invalid_hi_download = Recipe::Download(DownloadRecipe {
        hash: hello_hash.clone(),
        url: format!("{server_url}/hi.txt").parse().unwrap(),
    });

    let hi_download = Recipe::Download(DownloadRecipe {
        hash: hi_hash.clone(),
        url: format!("{server_url}/hi.txt").parse().unwrap(),
    });

    assert_eq!(
        bake_without_meta(&brioche, hello_download.clone()).await?,
        brioche_test::file(hello_blob, false),
    );

    assert_matches!(
        bake_without_meta(&brioche, invalid_hi_download.clone()).await,
        Err(_)
    );

    assert_eq!(
        bake_without_meta(&brioche, hi_download.clone()).await?,
        brioche_test::file(hi_blob, false),
    );

    hello_endpoint.assert();
    hi_endpoint.expect(2).assert();

    Ok(())
}
