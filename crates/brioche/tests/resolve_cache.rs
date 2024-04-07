use brioche::artifact::{DownloadArtifact, LazyArtifact};
use brioche_test::resolve_without_meta;

mod brioche_test;

#[tokio::test]
async fn test_resolve_cache_nested() -> anyhow::Result<()> {
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

    let hello_download = LazyArtifact::Download(DownloadArtifact {
        hash: hello_hash.clone(),
        url: format!("{server_url}/file.txt").parse().unwrap(),
    });

    let hello_nested_download = brioche_test::lazy_dir([("file.txt", hello_download.clone())]);

    assert_eq!(
        resolve_without_meta(&brioche, hello_download.clone()).await?,
        brioche_test::file(hello_blob, false),
    );

    // The cache from hello_download should be re-used
    assert_eq!(
        resolve_without_meta(&brioche, hello_nested_download).await?,
        brioche_test::dir(
            &brioche,
            [("file.txt", brioche_test::file(hello_blob, false))]
        )
        .await,
    );

    hello_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_resolve_cache_unnested() -> anyhow::Result<()> {
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

    let hello_download = LazyArtifact::Download(DownloadArtifact {
        hash: hello_hash.clone(),
        url: format!("{server_url}/file.txt").parse().unwrap(),
    });

    let hello_nested_download = brioche_test::lazy_dir([("file.txt", hello_download.clone())]);

    assert_eq!(
        resolve_without_meta(&brioche, hello_nested_download).await?,
        brioche_test::dir(
            &brioche,
            [("file.txt", brioche_test::file(hello_blob, false))]
        )
        .await,
    );

    // The cache from hello_nested_download should be re-used
    assert_eq!(
        resolve_without_meta(&brioche, hello_download.clone()).await?,
        brioche_test::file(hello_blob, false),
    );

    hello_endpoint.assert();

    Ok(())
}
