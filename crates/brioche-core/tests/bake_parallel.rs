use brioche_core::recipe::{DownloadRecipe, Recipe};
use brioche_test::bake_without_meta;

mod brioche_test;

#[tokio::test]
async fn test_bake_parallel_no_duplicates() -> anyhow::Result<()> {
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

    let download_1 = bake_without_meta(&brioche, hello_download.clone());
    let download_2 = bake_without_meta(&brioche, hello_download);

    let (download_result_1, download_result_2) = tokio::join!(download_1, download_2);

    assert_eq!(download_result_1?, brioche_test::file(hello_blob, false));
    assert_eq!(download_result_2?, brioche_test::file(hello_blob, false));

    // Ensure we only downloaded the file once
    hello_endpoint.assert();

    Ok(())
}
