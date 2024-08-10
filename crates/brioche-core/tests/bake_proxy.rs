use brioche_core::{bake::create_proxy, recipe::Recipe};
use brioche_test_support::bake_without_meta;

#[tokio::test]
async fn test_bake_proxy() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test_support::brioche_test().await;

    let hello_blob = brioche_test_support::blob(&brioche, "hello").await;

    let merge = Recipe::Merge {
        directories: vec![
            brioche_test_support::without_meta(brioche_test_support::lazy_dir_empty()),
            brioche_test_support::without_meta(brioche_test_support::lazy_dir([(
                "hello",
                brioche_test_support::lazy_file(hello_blob, false),
            )])),
        ],
    };

    let merge_proxy = create_proxy(&brioche, merge.clone()).await?;

    // The hash of the proxy recipe should be different from the hash of the
    // recipe it wraps
    assert_ne!(merge_proxy.hash(), merge.hash());

    let merge_proxy_result = bake_without_meta(&brioche, merge_proxy).await?;
    assert_eq!(
        merge_proxy_result,
        brioche_test_support::dir(
            &brioche,
            [("hello", brioche_test_support::file(hello_blob, false))]
        )
        .await,
    );

    Ok(())
}
