use std::collections::BTreeMap;

use assert_matches::assert_matches;
use brioche_core::recipe::{Artifact, WithMeta};

#[tokio::test]
async fn test_directory_create_empty() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let directory = brioche_core::recipe::Directory::create(&brioche, &BTreeMap::new()).await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir_empty()
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_create_flat() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let directory = brioche_core::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("file1.txt".into(), WithMeta::without_meta(file1)),
            ("file2.txt".into(), WithMeta::without_meta(file2)),
        ]),
    )
    .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [
                ("file1.txt", brioche_test_support::file(blob1, false)),
                ("file2.txt", brioche_test_support::file(blob2, false)),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_create_nested() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let inner_directory = brioche_core::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("file1.txt".into(), WithMeta::without_meta(file1.clone())),
            ("file2.txt".into(), WithMeta::without_meta(file2.clone())),
        ]),
    )
    .await?;

    let directory = brioche_core::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("foo.txt".into(), WithMeta::without_meta(file1)),
            ("bar.txt".into(), WithMeta::without_meta(file2)),
            (
                "subdir".into(),
                WithMeta::without_meta(Artifact::Directory(inner_directory)),
            ),
        ]),
    )
    .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [
                ("foo.txt", brioche_test_support::file(blob1, false)),
                ("bar.txt", brioche_test_support::file(blob2, false)),
                (
                    "subdir",
                    brioche_test_support::dir(
                        &brioche,
                        [
                            ("file1.txt", brioche_test_support::file(blob1, false)),
                            ("file2.txt", brioche_test_support::file(blob2, false))
                        ]
                    )
                    .await
                ),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_create_nested_using_paths() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let directory = brioche_core::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("foo.txt".into(), WithMeta::without_meta(file1.clone())),
            ("bar.txt".into(), WithMeta::without_meta(file2.clone())),
            ("subdir/file1.txt".into(), WithMeta::without_meta(file1)),
            ("subdir/file2.txt".into(), WithMeta::without_meta(file2)),
        ]),
    )
    .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [
                ("foo.txt", brioche_test_support::file(blob1, false)),
                ("bar.txt", brioche_test_support::file(blob2, false)),
                (
                    "subdir",
                    brioche_test_support::dir(
                        &brioche,
                        [
                            ("file1.txt", brioche_test_support::file(blob1, false)),
                            ("file2.txt", brioche_test_support::file(blob2, false))
                        ]
                    )
                    .await
                ),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_create_nested_with_common_empty_dir() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let directory = brioche_core::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("foo.txt".into(), WithMeta::without_meta(file1.clone())),
            ("bar.txt".into(), WithMeta::without_meta(file2.clone())),
            (
                "subdir".into(),
                WithMeta::without_meta(Artifact::Directory(
                    brioche_core::recipe::Directory::default(),
                )),
            ),
            ("subdir/file1.txt".into(), WithMeta::without_meta(file1)),
            ("subdir/file2.txt".into(), WithMeta::without_meta(file2)),
        ]),
    )
    .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [
                ("foo.txt", brioche_test_support::file(blob1, false)),
                ("bar.txt", brioche_test_support::file(blob2, false)),
                (
                    "subdir",
                    brioche_test_support::dir(
                        &brioche,
                        [
                            ("file1.txt", brioche_test_support::file(blob1, false)),
                            ("file2.txt", brioche_test_support::file(blob2, false))
                        ]
                    )
                    .await
                ),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_create_nested_with_common_nonempty_dir_error() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let inner_directory = brioche_core::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("file1.txt".into(), WithMeta::without_meta(file1.clone())),
            ("file2.txt".into(), WithMeta::without_meta(file2.clone())),
        ]),
    )
    .await?;

    let result = brioche_core::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("foo.txt".into(), WithMeta::without_meta(file1.clone())),
            ("bar.txt".into(), WithMeta::without_meta(file2.clone())),
            (
                "subdir".into(),
                WithMeta::without_meta(Artifact::Directory(inner_directory)),
            ),
            ("subdir/file1.txt".into(), WithMeta::without_meta(file1)),
            ("subdir/file2.txt".into(), WithMeta::without_meta(file2)),
        ]),
    )
    .await;

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_directory_create_nested_with_common_nondir_error() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let result = brioche_core::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("foo.txt".into(), WithMeta::without_meta(file1.clone())),
            ("bar.txt".into(), WithMeta::without_meta(file2.clone())),
            ("subdir".into(), WithMeta::without_meta(file2.clone())),
            ("subdir/file1.txt".into(), WithMeta::without_meta(file1)),
            ("subdir/file2.txt".into(), WithMeta::without_meta(file2)),
        ]),
    )
    .await;

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_new() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let mut directory =
        brioche_test_support::dir_value(&brioche, [("file1.txt", file1.clone())]).await;

    directory
        .insert(&brioche, b"file2.txt", Some(file2.clone()))
        .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [
                ("file1.txt", brioche_test_support::file(blob1, false)),
                ("file2.txt", brioche_test_support::file(blob2, false))
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_replace() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob_old = brioche_test_support::blob(&brioche, "hello world").await;
    let file_old = brioche_test_support::file(blob_old, false);

    let blob_new = brioche_test_support::blob(&brioche, "hi").await;
    let file_new = brioche_test_support::file(blob_new, false);

    let mut directory =
        brioche_test_support::dir_value(&brioche, [("file1.txt", file_old.clone())]).await;

    directory
        .insert(&brioche, b"file1.txt", Some(file_new.clone()))
        .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [("file1.txt", brioche_test_support::file(blob_new, false))]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_remove() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let mut directory = brioche_test_support::dir_value(
        &brioche,
        [("file1.txt", file1.clone()), ("file2.txt", file2.clone())],
    )
    .await;

    directory.insert(&brioche, b"file2.txt", None).await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [("file1.txt", brioche_test_support::file(blob1, false))]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_remove_no_op() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let mut directory = brioche_test_support::dir_value(
        &brioche,
        [("file1.txt", file1.clone()), ("file2.txt", file2.clone())],
    )
    .await;

    directory.insert(&brioche, b"file3.txt", None).await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [
                ("file1.txt", brioche_test_support::file(blob1, false)),
                ("file2.txt", brioche_test_support::file(blob2, false)),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_new_nested() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let mut directory = brioche_test_support::dir_value(
        &brioche,
        [(
            "subdir",
            brioche_test_support::dir(&brioche, [("file1.txt", file1.clone())]).await,
        )],
    )
    .await;

    directory
        .insert(&brioche, b"subdir/file2.txt", Some(file2.clone()))
        .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [(
                "subdir",
                brioche_test_support::dir(
                    &brioche,
                    [
                        ("file1.txt", brioche_test_support::file(blob1, false)),
                        ("file2.txt", brioche_test_support::file(blob2, false)),
                    ]
                )
                .await
            ),]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_new_dir() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let mut directory =
        brioche_test_support::dir_value(&brioche, [("file1.txt", file1.clone())]).await;

    directory
        .insert(&brioche, b"subdir/file2.txt", Some(file2.clone()))
        .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [
                ("file1.txt", brioche_test_support::file(blob1, false)),
                (
                    "subdir",
                    brioche_test_support::dir(
                        &brioche,
                        [("file2.txt", brioche_test_support::file(blob2, false)),]
                    )
                    .await
                ),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_replace_nested() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let mut directory = brioche_test_support::dir_value(
        &brioche,
        [(
            "foo",
            brioche_test_support::dir(
                &brioche,
                [(
                    "bar",
                    brioche_test_support::dir(&brioche, [("file1.txt", file1.clone())]).await,
                )],
            )
            .await,
        )],
    )
    .await;

    directory
        .insert(&brioche, b"foo/bar", Some(file2.clone()))
        .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [(
                "foo",
                brioche_test_support::dir(
                    &brioche,
                    [("bar", brioche_test_support::file(blob2, false)),]
                )
                .await
            ),]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_remove_nested() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let mut directory = brioche_test_support::dir_value(
        &brioche,
        [(
            "foo",
            brioche_test_support::dir(
                &brioche,
                [(
                    "bar",
                    brioche_test_support::dir(&brioche, [("file1.txt", file1.clone())]).await,
                )],
            )
            .await,
        )],
    )
    .await;

    directory
        .insert(&brioche, b"foo/bar/file1.txt", None)
        .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [(
                "foo",
                brioche_test_support::dir(&brioche, [("bar", brioche_test_support::dir_empty())])
                    .await
            ),]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_remove_nested_no_op() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let mut directory = brioche_test_support::dir_value(
        &brioche,
        [(
            "foo",
            brioche_test_support::dir(
                &brioche,
                [(
                    "bar",
                    brioche_test_support::dir(
                        &brioche,
                        [("file1.txt", file1.clone()), ("file2.txt", file2.clone())],
                    )
                    .await,
                )],
            )
            .await,
        )],
    )
    .await;

    directory
        .insert(&brioche, b"foo/bar/file3.txt", None)
        .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [(
                "foo",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "bar",
                        brioche_test_support::dir(
                            &brioche,
                            [
                                ("file1.txt", brioche_test_support::file(blob1, false)),
                                ("file2.txt", brioche_test_support::file(blob2, false)),
                            ]
                        )
                        .await
                    ),]
                )
                .await
            ),]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_insert_descends_into_non_dir_error() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "hello world").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "hi").await;
    let file2 = brioche_test_support::file(blob2, false);

    let mut directory = brioche_test_support::dir_value(
        &brioche,
        [(
            "foo",
            brioche_test_support::dir(
                &brioche,
                [(
                    "bar",
                    brioche_test_support::dir(
                        &brioche,
                        [("file1.txt", file1.clone()), ("file2.txt", file2.clone())],
                    )
                    .await,
                )],
            )
            .await,
        )],
    )
    .await;

    let result = directory
        .insert(&brioche, b"foo/bar/file1.txt/asdf", None)
        .await;

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_directory_merge() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let blob1 = brioche_test_support::blob(&brioche, "blob 1").await;
    let file1 = brioche_test_support::file(blob1, false);

    let blob2 = brioche_test_support::blob(&brioche, "blob 2").await;
    let file2 = brioche_test_support::file(blob2, false);

    let blob3 = brioche_test_support::blob(&brioche, "blob 3").await;
    let file3 = brioche_test_support::file(blob3, false);

    let mut directory = brioche_test_support::dir_value(
        &brioche,
        [
            ("top_a", file1.clone()),
            (
                "top_b",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "nested",
                        brioche_test_support::dir(
                            &brioche,
                            [("a.txt", file1.clone()), ("b.txt", file2.clone())],
                        )
                        .await,
                    )],
                )
                .await,
            ),
            (
                "top_c",
                brioche_test_support::dir(&brioche, [("file3.txt", file3.clone())]).await,
            ),
        ],
    )
    .await;
    let other = brioche_test_support::dir_value(
        &brioche,
        [
            (
                "top_b",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "nested",
                        brioche_test_support::dir(
                            &brioche,
                            [("b.txt", file3.clone()), ("c.txt", file3.clone())],
                        )
                        .await,
                    )],
                )
                .await,
            ),
            ("top_c", file1.clone()),
            ("top_d", brioche_test_support::dir_empty()),
        ],
    )
    .await;

    directory.merge(&other, &brioche).await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test_support::dir(
            &brioche,
            [
                ("top_a", file1.clone()),
                (
                    "top_b",
                    brioche_test_support::dir(
                        &brioche,
                        [(
                            "nested",
                            brioche_test_support::dir(
                                &brioche,
                                [
                                    ("a.txt", file1.clone()),
                                    ("b.txt", file3.clone()),
                                    ("c.txt", file3.clone())
                                ],
                            )
                            .await,
                        )],
                    )
                    .await,
                ),
                ("top_c", file1.clone()),
                ("top_d", brioche_test_support::dir_empty()),
            ]
        )
        .await
    );

    Ok(())
}
