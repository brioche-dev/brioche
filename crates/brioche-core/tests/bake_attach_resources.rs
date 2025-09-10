use assert_matches::assert_matches;
use pretty_assertions::assert_eq;

use brioche_core::{blob::BlobHash, recipe::Recipe};
use brioche_test_support::{bake_without_meta, brioche_test, without_meta};

async fn blob_with_resource_paths(
    brioche: &brioche_core::Brioche,
    content: impl AsRef<[u8]>,
    resource_paths: impl IntoIterator<Item = impl AsRef<[u8]>>,
) -> BlobHash {
    let mut content = content.as_ref().to_vec();
    let resource_paths = resource_paths
        .into_iter()
        .map(|path| path.as_ref().to_vec())
        .collect();
    brioche_pack::inject_pack(
        &mut content,
        &brioche_pack::Pack::Metadata {
            resource_paths,
            format: "test".into(),
            metadata: vec![],
        },
    )
    .expect("failed to inject pack");

    brioche_test_support::blob(brioche, content).await
}

#[expect(clippy::similar_names)]
#[tokio::test]
async fn test_bake_attach_resources_without_resources() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test().await;

    let foo_blob = brioche_test_support::blob(&brioche, b"foo").await;
    let bar_blob = brioche_test_support::blob(&brioche, b"bar").await;
    let baz_blob = brioche_test_support::blob(&brioche, b"baz").await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            ("foo.txt", brioche_test_support::file(foo_blob, false)),
            ("bar.txt", brioche_test_support::file(bar_blob, true)),
            (
                "brioche-resources.d",
                brioche_test_support::symlink("../brioche-resources.d"),
            ),
            (
                "dir",
                brioche_test_support::dir(
                    &brioche,
                    [("baz.txt", brioche_test_support::file(baz_blob, false))],
                )
                .await,
            ),
        ],
    )
    .await;
    let recipe = Recipe::AttachResources {
        recipe: Box::new(without_meta(dir.clone().into())),
    };

    let output = bake_without_meta(&brioche, recipe).await?;
    assert_eq!(output, dir);

    Ok(())
}

#[expect(clippy::similar_names)]
#[tokio::test]
async fn test_bake_attach_resources_add_all_resources() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test().await;

    let foo_blob = blob_with_resource_paths(&brioche, b"foo", ["fizz/a"]).await;
    let bar_blob = blob_with_resource_paths(&brioche, b"bar", ["fizz/b", "fizz/c", "d.txt"]).await;
    let baz_blob = blob_with_resource_paths(&brioche, b"baz", ["fizz/c", "buzz", "e.lnk"]).await;

    let a_blob = brioche_test_support::blob(&brioche, b"a").await;
    let b_blob = brioche_test_support::blob(&brioche, b"b").await;
    let c_blob = brioche_test_support::blob(&brioche, b"c").await;
    let d_blob = brioche_test_support::blob(&brioche, b"d").await;
    let e_blob = brioche_test_support::blob(&brioche, b"e").await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            ("foo.txt", brioche_test_support::file(foo_blob, false)),
            ("bar.txt", brioche_test_support::file(bar_blob, true)),
            ("dir/baz.txt", brioche_test_support::file(baz_blob, false)),
            (
                "brioche-resources.d",
                brioche_test_support::dir(
                    &brioche,
                    [
                        ("fizz/a", brioche_test_support::file(a_blob, false)),
                        ("fizz/b", brioche_test_support::file(b_blob, true)),
                        ("fizz/c", brioche_test_support::file(c_blob, false)),
                        ("buzz/a.txt", brioche_test_support::symlink("../fizz/a")),
                        (
                            "buzz/b.txt",
                            brioche_test_support::symlink("../fizz/broken.txt"),
                        ),
                        ("buzz/d.txt", brioche_test_support::symlink("../d.txt")),
                        ("d.txt", brioche_test_support::file(d_blob, false)),
                        ("d.lnk", brioche_test_support::symlink("d.txt")),
                        ("e.txt", brioche_test_support::file(e_blob, false)),
                        ("e.lnk", brioche_test_support::symlink("e.txt")),
                    ],
                )
                .await,
            ),
        ],
    )
    .await;
    let recipe = Recipe::AttachResources {
        recipe: Box::new(without_meta(dir.clone().into())),
    };

    let expected_output = brioche_test_support::dir(
        &brioche,
        [
            (
                "foo.txt",
                brioche_test_support::file_with_resources(
                    foo_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [("fizz/a", brioche_test_support::file(a_blob, false))],
                    )
                    .await,
                ),
            ),
            (
                "bar.txt",
                brioche_test_support::file_with_resources(
                    bar_blob,
                    true,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("fizz/b", brioche_test_support::file(b_blob, true)),
                            ("fizz/c", brioche_test_support::file(c_blob, false)),
                            ("d.txt", brioche_test_support::file(d_blob, false)),
                        ],
                    )
                    .await,
                ),
            ),
            (
                "dir",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "baz.txt",
                        brioche_test_support::file_with_resources(
                            baz_blob,
                            false,
                            brioche_test_support::dir_value(
                                &brioche,
                                [
                                    ("fizz/a", brioche_test_support::file(a_blob, false)),
                                    ("fizz/c", brioche_test_support::file(c_blob, false)),
                                    ("buzz/a.txt", brioche_test_support::symlink("../fizz/a")),
                                    (
                                        "buzz/b.txt",
                                        brioche_test_support::symlink("../fizz/broken.txt"),
                                    ),
                                    ("buzz/d.txt", brioche_test_support::symlink("../d.txt")),
                                    ("d.txt", brioche_test_support::file(d_blob, false)),
                                    ("e.txt", brioche_test_support::file(e_blob, false)),
                                    ("e.lnk", brioche_test_support::symlink("e.txt")),
                                ],
                            )
                            .await,
                        ),
                    )],
                )
                .await,
            ),
            (
                "brioche-resources.d",
                brioche_test_support::dir(
                    &brioche,
                    [
                        ("fizz/a", brioche_test_support::file(a_blob, false)),
                        ("fizz/b", brioche_test_support::file(b_blob, true)),
                        ("fizz/c", brioche_test_support::file(c_blob, false)),
                        ("buzz/a.txt", brioche_test_support::symlink("../fizz/a")),
                        (
                            "buzz/b.txt",
                            brioche_test_support::symlink("../fizz/broken.txt"),
                        ),
                        ("buzz/d.txt", brioche_test_support::symlink("../d.txt")),
                        ("d.txt", brioche_test_support::file(d_blob, false)),
                        ("d.lnk", brioche_test_support::symlink("d.txt")),
                        ("e.txt", brioche_test_support::file(e_blob, false)),
                        ("e.lnk", brioche_test_support::symlink("e.txt")),
                    ],
                )
                .await,
            ),
        ],
    )
    .await;

    let output = bake_without_meta(&brioche, recipe).await?;

    assert_eq!(output, expected_output);

    Ok(())
}

#[expect(clippy::similar_names)]
#[tokio::test]
async fn test_bake_attach_resources_keep_existing_resources() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test().await;

    let foo_blob = brioche_test_support::blob(&brioche, b"foo").await;
    let bar_blob = brioche_test_support::blob(&brioche, b"bar").await;
    let baz_blob = brioche_test_support::blob(&brioche, b"baz").await;

    let a_blob = brioche_test_support::blob(&brioche, b"a").await;
    let b_blob = brioche_test_support::blob(&brioche, b"b").await;
    let c_blob = brioche_test_support::blob(&brioche, b"c").await;
    let d_blob = brioche_test_support::blob(&brioche, b"d").await;
    let e_blob = brioche_test_support::blob(&brioche, b"e").await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            (
                "foo.txt",
                brioche_test_support::file_with_resources(
                    foo_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [("fizz/a", brioche_test_support::file(a_blob, false))],
                    )
                    .await,
                ),
            ),
            (
                "bar.txt",
                brioche_test_support::file_with_resources(
                    bar_blob,
                    true,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("fizz/b", brioche_test_support::file(b_blob, true)),
                            ("fizz/c", brioche_test_support::file(c_blob, false)),
                            ("d.txt", brioche_test_support::file(d_blob, false)),
                        ],
                    )
                    .await,
                ),
            ),
            (
                "dir",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "baz.txt",
                        brioche_test_support::file_with_resources(
                            baz_blob,
                            false,
                            brioche_test_support::dir_value(
                                &brioche,
                                [
                                    ("fizz/a", brioche_test_support::file(a_blob, false)),
                                    ("fizz/c", brioche_test_support::file(c_blob, false)),
                                    ("buzz/a.txt", brioche_test_support::symlink("../fizz/a")),
                                    (
                                        "buzz/b.txt",
                                        brioche_test_support::symlink("../fizz/broken.txt"),
                                    ),
                                    ("buzz/d.txt", brioche_test_support::symlink("../d.txt")),
                                    ("d.txt", brioche_test_support::file(d_blob, false)),
                                    ("e.txt", brioche_test_support::file(e_blob, false)),
                                    ("e.lnk", brioche_test_support::symlink("e.txt")),
                                ],
                            )
                            .await,
                        ),
                    )],
                )
                .await,
            ),
            (
                "brioche-resources.d",
                brioche_test_support::dir(
                    &brioche,
                    [
                        ("fizz/a", brioche_test_support::file(a_blob, false)),
                        ("fizz/b", brioche_test_support::file(b_blob, true)),
                        ("fizz/c", brioche_test_support::file(c_blob, false)),
                        ("buzz/a.txt", brioche_test_support::symlink("../fizz/a")),
                        (
                            "buzz/b.txt",
                            brioche_test_support::symlink("../fizz/broken.txt"),
                        ),
                        ("buzz/d.txt", brioche_test_support::symlink("../d.txt")),
                        ("d.txt", brioche_test_support::file(d_blob, false)),
                        ("d.lnk", brioche_test_support::symlink("d.txt")),
                        ("e.txt", brioche_test_support::file(e_blob, false)),
                        ("e.lnk", brioche_test_support::symlink("e.txt")),
                    ],
                )
                .await,
            ),
        ],
    )
    .await;

    let recipe = Recipe::AttachResources {
        recipe: Box::new(without_meta(dir.clone().into())),
    };

    let output = bake_without_meta(&brioche, recipe).await?;

    assert_eq!(output, dir);

    Ok(())
}

#[expect(clippy::similar_names)]
#[tokio::test]
async fn test_bake_attach_resources_layers() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test().await;

    let foo_blob = blob_with_resource_paths(&brioche, b"foo", ["a"]).await;
    let bar_blob = blob_with_resource_paths(&brioche, b"bar", ["b", "c"]).await;
    let baz_blob = blob_with_resource_paths(&brioche, b"baz", ["a", "c"]).await;

    let a1_blob = brioche_test_support::blob(&brioche, b"a1").await;
    let b1_blob = brioche_test_support::blob(&brioche, b"b1").await;
    let c1_blob = brioche_test_support::blob(&brioche, b"c1").await;

    let a2_blob = brioche_test_support::blob(&brioche, b"a2").await;
    let b2_blob = brioche_test_support::blob(&brioche, b"b2").await;
    let c2_blob = brioche_test_support::blob(&brioche, b"c2").await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            ("foo.txt", brioche_test_support::file(foo_blob, false)),
            ("bar.txt", brioche_test_support::file(bar_blob, false)),
            ("baz.txt", brioche_test_support::file(baz_blob, false)),
            ("fizz/foo.txt", brioche_test_support::file(foo_blob, false)),
            ("fizz/bar.txt", brioche_test_support::file(bar_blob, false)),
            ("fizz/baz.txt", brioche_test_support::file(baz_blob, false)),
            (
                "fizz/buzz/foo.txt",
                brioche_test_support::file(foo_blob, false),
            ),
            (
                "fizz/buzz/bar.txt",
                brioche_test_support::file(bar_blob, false),
            ),
            (
                "fizz/buzz/baz.txt",
                brioche_test_support::file(baz_blob, false),
            ),
            (
                "brioche-resources.d/a",
                brioche_test_support::file(a1_blob, false),
            ),
            (
                "brioche-resources.d/b",
                brioche_test_support::file(b1_blob, false),
            ),
            (
                "brioche-resources.d/c",
                brioche_test_support::file(c1_blob, false),
            ),
            (
                "fizz/brioche-resources.d/b",
                brioche_test_support::file(b2_blob, false),
            ),
            (
                "fizz/buzz/brioche-resources.d/a",
                brioche_test_support::file(a2_blob, false),
            ),
            (
                "fizz/buzz/brioche-resources.d/c",
                brioche_test_support::file(c2_blob, false),
            ),
        ],
    )
    .await;
    let recipe = Recipe::AttachResources {
        recipe: Box::new(without_meta(dir.clone().into())),
    };

    let expected_output = brioche_test_support::dir(
        &brioche,
        [
            (
                "foo.txt",
                brioche_test_support::file_with_resources(
                    foo_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [("a", brioche_test_support::file(a1_blob, false))],
                    )
                    .await,
                ),
            ),
            (
                "bar.txt",
                brioche_test_support::file_with_resources(
                    bar_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("b", brioche_test_support::file(b1_blob, false)),
                            ("c", brioche_test_support::file(c1_blob, false)),
                        ],
                    )
                    .await,
                ),
            ),
            (
                "baz.txt",
                brioche_test_support::file_with_resources(
                    baz_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("a", brioche_test_support::file(a1_blob, false)),
                            ("c", brioche_test_support::file(c1_blob, false)),
                        ],
                    )
                    .await,
                ),
            ),
            (
                "fizz/foo.txt",
                brioche_test_support::file_with_resources(
                    foo_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [("a", brioche_test_support::file(a1_blob, false))],
                    )
                    .await,
                ),
            ),
            (
                "fizz/bar.txt",
                brioche_test_support::file_with_resources(
                    bar_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("b", brioche_test_support::file(b2_blob, false)),
                            ("c", brioche_test_support::file(c1_blob, false)),
                        ],
                    )
                    .await,
                ),
            ),
            (
                "fizz/baz.txt",
                brioche_test_support::file_with_resources(
                    baz_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("a", brioche_test_support::file(a1_blob, false)),
                            ("c", brioche_test_support::file(c1_blob, false)),
                        ],
                    )
                    .await,
                ),
            ),
            (
                "fizz/buzz/foo.txt",
                brioche_test_support::file_with_resources(
                    foo_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [("a", brioche_test_support::file(a2_blob, false))],
                    )
                    .await,
                ),
            ),
            (
                "fizz/buzz/bar.txt",
                brioche_test_support::file_with_resources(
                    bar_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("b", brioche_test_support::file(b2_blob, false)),
                            ("c", brioche_test_support::file(c2_blob, false)),
                        ],
                    )
                    .await,
                ),
            ),
            (
                "fizz/buzz/baz.txt",
                brioche_test_support::file_with_resources(
                    baz_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("a", brioche_test_support::file(a2_blob, false)),
                            ("c", brioche_test_support::file(c2_blob, false)),
                        ],
                    )
                    .await,
                ),
            ),
            (
                "brioche-resources.d/a",
                brioche_test_support::file(a1_blob, false),
            ),
            (
                "brioche-resources.d/b",
                brioche_test_support::file(b1_blob, false),
            ),
            (
                "brioche-resources.d/c",
                brioche_test_support::file(c1_blob, false),
            ),
            (
                "fizz/brioche-resources.d/b",
                brioche_test_support::file(b2_blob, false),
            ),
            (
                "fizz/buzz/brioche-resources.d/a",
                brioche_test_support::file(a2_blob, false),
            ),
            (
                "fizz/buzz/brioche-resources.d/c",
                brioche_test_support::file(c2_blob, false),
            ),
        ],
    )
    .await;

    let output = bake_without_meta(&brioche, recipe).await?;

    assert_eq!(output, expected_output);

    Ok(())
}

#[tokio::test]
async fn test_bake_attach_resources_with_external_resources() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test().await;

    let top_blob = blob_with_resource_paths(&brioche, b"top", ["fizz"]).await;
    let fizz_blob = blob_with_resource_paths(&brioche, b"fizz", ["buzz"]).await;
    let buzz_blob = brioche_test_support::blob(&brioche, b"buzz!").await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            (
                "inside/top.txt",
                brioche_test_support::file(top_blob, false),
            ),
            (
                "inside/brioche-resources.d/fizz",
                brioche_test_support::file(fizz_blob, false),
            ),
            (
                "brioche-resources.d/buzz",
                brioche_test_support::file(buzz_blob, false),
            ),
        ],
    )
    .await;
    let recipe = Recipe::AttachResources {
        recipe: Box::new(without_meta(dir.clone().into())),
    };

    let expected_output = brioche_test_support::dir(
        &brioche,
        [
            (
                "inside/top.txt",
                brioche_test_support::file_with_resources(
                    top_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [(
                            "fizz",
                            brioche_test_support::file_with_resources(
                                fizz_blob,
                                false,
                                brioche_test_support::dir_value(
                                    &brioche,
                                    [("buzz", brioche_test_support::file(buzz_blob, false))],
                                )
                                .await,
                            ),
                        )],
                    )
                    .await,
                ),
            ),
            (
                "inside/brioche-resources.d/fizz",
                brioche_test_support::file_with_resources(
                    fizz_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [("buzz", brioche_test_support::file(buzz_blob, false))],
                    )
                    .await,
                ),
            ),
            (
                "brioche-resources.d/buzz",
                brioche_test_support::file(buzz_blob, false),
            ),
        ],
    )
    .await;

    let output = bake_without_meta(&brioche, recipe).await?;

    assert_eq!(output, expected_output);

    Ok(())
}

#[tokio::test]
async fn test_bake_attach_resources_with_internal_resources() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test().await;

    let top_blob = blob_with_resource_paths(&brioche, b"top", ["fizz"]).await;
    let fizz_blob = blob_with_resource_paths(&brioche, b"fizz", ["buzz"]).await;
    let buzz_blob = brioche_test_support::blob(&brioche, b"buzz!").await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            ("top.txt", brioche_test_support::file(top_blob, false)),
            (
                "brioche-resources.d/fizz",
                brioche_test_support::file(fizz_blob, false),
            ),
            (
                "brioche-resources.d/buzz",
                brioche_test_support::file(buzz_blob, false),
            ),
        ],
    )
    .await;
    let recipe = Recipe::AttachResources {
        recipe: Box::new(without_meta(dir.clone().into())),
    };

    let expected_output = brioche_test_support::dir(
        &brioche,
        [
            (
                "top.txt",
                brioche_test_support::file_with_resources(
                    top_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [(
                            "fizz",
                            brioche_test_support::file_with_resources(
                                fizz_blob,
                                false,
                                brioche_test_support::dir_value(
                                    &brioche,
                                    [("buzz", brioche_test_support::file(buzz_blob, false))],
                                )
                                .await,
                            ),
                        )],
                    )
                    .await,
                ),
            ),
            (
                "brioche-resources.d/fizz",
                brioche_test_support::file_with_resources(
                    fizz_blob,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [("buzz", brioche_test_support::file(buzz_blob, false))],
                    )
                    .await,
                ),
            ),
            (
                "brioche-resources.d/buzz",
                brioche_test_support::file(buzz_blob, false),
            ),
        ],
    )
    .await;

    let output = bake_without_meta(&brioche, recipe).await?;

    assert_eq!(output, expected_output);

    Ok(())
}

#[tokio::test]
async fn test_bake_attach_resources_with_recursive_resource_error() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test().await;

    let top_blob = blob_with_resource_paths(&brioche, b"top", ["fizz"]).await;
    let fizz_blob = blob_with_resource_paths(&brioche, b"fizz", ["fizz"]).await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            ("top.txt", brioche_test_support::file(top_blob, false)),
            (
                "brioche-resources.d/fizz",
                brioche_test_support::file(fizz_blob, false),
            ),
        ],
    )
    .await;
    let recipe = Recipe::AttachResources {
        recipe: Box::new(without_meta(dir.clone().into())),
    };

    let result = bake_without_meta(&brioche, recipe).await;

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_attach_resources_with_mutually_recursive_resource_error() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test().await;

    let top_blob = blob_with_resource_paths(&brioche, b"top", ["fizz"]).await;
    let fizz_blob = blob_with_resource_paths(&brioche, b"fizz", ["buzz"]).await;
    let buzz_blob = blob_with_resource_paths(&brioche, b"buzz", ["fizz"]).await;

    let dir = brioche_test_support::dir(
        &brioche,
        [
            ("top.txt", brioche_test_support::file(top_blob, false)),
            (
                "brioche-resources.d/fizz",
                brioche_test_support::file(fizz_blob, false),
            ),
            (
                "brioche-resources.d/buzz",
                brioche_test_support::file(buzz_blob, false),
            ),
        ],
    )
    .await;
    let recipe = Recipe::AttachResources {
        recipe: Box::new(without_meta(dir.clone().into())),
    };

    let result = bake_without_meta(&brioche, recipe).await;

    assert_matches!(result, Err(_));

    Ok(())
}
