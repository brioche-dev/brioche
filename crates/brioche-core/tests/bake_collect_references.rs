use brioche_core::recipe::{Recipe, WithMeta};

#[tokio::test]
async fn test_bake_collect_references() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test_support::brioche_test().await;

    let foo_blob = brioche_test_support::blob(&brioche, b"foo").await;
    let bar_blob = brioche_test_support::blob(&brioche, b"bar").await;
    let resource_a_blob = brioche_test_support::blob(&brioche, b"resource a").await;
    let resource_b_blob = brioche_test_support::blob(&brioche, b"resource b").await;
    let resource_c_blob = brioche_test_support::blob(&brioche, b"resource c").await;

    let resource_a = brioche_test_support::file(resource_a_blob, false);
    let resource_b = brioche_test_support::file(resource_b_blob, false);
    let resource_c = brioche_test_support::file_with_resources(
        resource_c_blob,
        false,
        brioche_test_support::dir_value(&brioche, [("foo/a.txt", resource_a.clone())]).await,
    );

    let fizz = brioche_test_support::dir(
        &brioche,
        [(
            "file.txt",
            brioche_test_support::file_with_resources(
                foo_blob,
                false,
                brioche_test_support::dir_value(&brioche, [("foo/b.txt", resource_b.clone())])
                    .await,
            ),
        )],
    )
    .await;

    let buzz = brioche_test_support::file_with_resources(
        brioche_test_support::blob(&brioche, b"bar").await,
        false,
        brioche_test_support::dir_value(&brioche, [("foo/c.txt", resource_c.clone())]).await,
    );

    let dir = brioche_test_support::dir(
        &brioche,
        [("fizz", fizz.clone()), ("buzz.txt", buzz.clone())],
    )
    .await;
    let recipe = Recipe::CollectReferences {
        recipe: Box::new(WithMeta::without_meta(dir.clone().into())),
    };

    let expected_output = brioche_test_support::dir(
        &brioche,
        [
            (
                "fizz",
                brioche_test_support::dir(
                    &brioche,
                    [("file.txt", brioche_test_support::file(foo_blob, false))],
                )
                .await,
            ),
            ("buzz.txt", brioche_test_support::file(bar_blob, false)),
            (
                "brioche-resources.d",
                brioche_test_support::dir(
                    &brioche,
                    [
                        (
                            "foo/a.txt",
                            brioche_test_support::file(resource_a_blob, false),
                        ),
                        (
                            "foo/b.txt",
                            brioche_test_support::file(resource_b_blob, false),
                        ),
                        (
                            "foo/c.txt",
                            brioche_test_support::file(resource_c_blob, false),
                        ),
                    ],
                )
                .await,
            ),
        ],
    )
    .await;

    let output = brioche_test_support::bake_without_meta(&brioche, recipe).await?;

    assert_eq!(output, expected_output);

    Ok(())
}

#[tokio::test]
async fn test_bake_collect_references_no_references() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test_support::brioche_test().await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            (
                "foo.txt",
                brioche_test_support::file(
                    brioche_test_support::blob(&brioche, "foo!").await,
                    false,
                ),
            ),
            (
                "bar/baz.txt",
                brioche_test_support::file(
                    brioche_test_support::blob(&brioche, "baz!").await,
                    false,
                ),
            ),
        ],
    )
    .await;

    let recipe = Recipe::CollectReferences {
        recipe: Box::new(WithMeta::without_meta(dir.clone().into())),
    };

    let output = brioche_test_support::bake_without_meta(&brioche, recipe).await?;

    assert_eq!(output, dir);

    Ok(())
}
