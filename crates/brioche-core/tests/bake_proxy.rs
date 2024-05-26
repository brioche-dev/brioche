use brioche_core::{bake::create_proxy, recipe::Recipe};
use brioche_test::bake_without_meta;

mod brioche_test;

#[tokio::test]
async fn test_bake_proxy() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, "hello").await;

    let merge = Recipe::Merge {
        directories: vec![
            brioche_test::without_meta(brioche_test::lazy_dir_empty()),
            brioche_test::without_meta(brioche_test::lazy_dir([(
                "hello",
                brioche_test::lazy_file(hello_blob, false),
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
        brioche_test::dir(&brioche, [("hello", brioche_test::file(hello_blob, false))]).await,
    );

    Ok(())
}
