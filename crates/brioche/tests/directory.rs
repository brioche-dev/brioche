use std::collections::BTreeMap;

use assert_matches::assert_matches;
use brioche::recipe::{Artifact, WithMeta};

mod brioche_test;

#[tokio::test]
async fn test_directory_create_empty() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let directory = brioche::recipe::Directory::create(&brioche, &BTreeMap::new()).await?;

    assert_eq!(Artifact::Directory(directory), brioche_test::dir_empty());

    Ok(())
}

#[tokio::test]
async fn test_directory_create_flat() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let blob1 = brioche_test::blob(&brioche, "hello world").await;
    let file1 = brioche_test::file(blob1, false);

    let blob2 = brioche_test::blob(&brioche, "hi").await;
    let file2 = brioche_test::file(blob2, false);

    let directory = brioche::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("file1.txt".into(), WithMeta::without_meta(file1)),
            ("file2.txt".into(), WithMeta::without_meta(file2)),
        ]),
    )
    .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test::dir(
            &brioche,
            [
                ("file1.txt", brioche_test::file(blob1, false)),
                ("file2.txt", brioche_test::file(blob2, false)),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_directory_create_nested() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let blob1 = brioche_test::blob(&brioche, "hello world").await;
    let file1 = brioche_test::file(blob1, false);

    let blob2 = brioche_test::blob(&brioche, "hi").await;
    let file2 = brioche_test::file(blob2, false);

    let inner_directory = brioche::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("file1.txt".into(), WithMeta::without_meta(file1.clone())),
            ("file2.txt".into(), WithMeta::without_meta(file2.clone())),
        ]),
    )
    .await?;

    let directory = brioche::recipe::Directory::create(
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
        brioche_test::dir(
            &brioche,
            [
                ("foo.txt", brioche_test::file(blob1, false)),
                ("bar.txt", brioche_test::file(blob2, false)),
                (
                    "subdir",
                    brioche_test::dir(
                        &brioche,
                        [
                            ("file1.txt", brioche_test::file(blob1, false)),
                            ("file2.txt", brioche_test::file(blob2, false))
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
    let (brioche, _) = brioche_test::brioche_test().await;

    let blob1 = brioche_test::blob(&brioche, "hello world").await;
    let file1 = brioche_test::file(blob1, false);

    let blob2 = brioche_test::blob(&brioche, "hi").await;
    let file2 = brioche_test::file(blob2, false);

    let directory = brioche::recipe::Directory::create(
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
        brioche_test::dir(
            &brioche,
            [
                ("foo.txt", brioche_test::file(blob1, false)),
                ("bar.txt", brioche_test::file(blob2, false)),
                (
                    "subdir",
                    brioche_test::dir(
                        &brioche,
                        [
                            ("file1.txt", brioche_test::file(blob1, false)),
                            ("file2.txt", brioche_test::file(blob2, false))
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
    let (brioche, _) = brioche_test::brioche_test().await;

    let blob1 = brioche_test::blob(&brioche, "hello world").await;
    let file1 = brioche_test::file(blob1, false);

    let blob2 = brioche_test::blob(&brioche, "hi").await;
    let file2 = brioche_test::file(blob2, false);

    let directory = brioche::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("foo.txt".into(), WithMeta::without_meta(file1.clone())),
            ("bar.txt".into(), WithMeta::without_meta(file2.clone())),
            (
                "subdir".into(),
                WithMeta::without_meta(Artifact::Directory(brioche::recipe::Directory::default())),
            ),
            ("subdir/file1.txt".into(), WithMeta::without_meta(file1)),
            ("subdir/file2.txt".into(), WithMeta::without_meta(file2)),
        ]),
    )
    .await?;

    assert_eq!(
        Artifact::Directory(directory),
        brioche_test::dir(
            &brioche,
            [
                ("foo.txt", brioche_test::file(blob1, false)),
                ("bar.txt", brioche_test::file(blob2, false)),
                (
                    "subdir",
                    brioche_test::dir(
                        &brioche,
                        [
                            ("file1.txt", brioche_test::file(blob1, false)),
                            ("file2.txt", brioche_test::file(blob2, false))
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
    let (brioche, _) = brioche_test::brioche_test().await;

    let blob1 = brioche_test::blob(&brioche, "hello world").await;
    let file1 = brioche_test::file(blob1, false);

    let blob2 = brioche_test::blob(&brioche, "hi").await;
    let file2 = brioche_test::file(blob2, false);

    let inner_directory = brioche::recipe::Directory::create(
        &brioche,
        &BTreeMap::from_iter([
            ("file1.txt".into(), WithMeta::without_meta(file1.clone())),
            ("file2.txt".into(), WithMeta::without_meta(file2.clone())),
        ]),
    )
    .await?;

    let result = brioche::recipe::Directory::create(
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
    let (brioche, _) = brioche_test::brioche_test().await;

    let blob1 = brioche_test::blob(&brioche, "hello world").await;
    let file1 = brioche_test::file(blob1, false);

    let blob2 = brioche_test::blob(&brioche, "hi").await;
    let file2 = brioche_test::file(blob2, false);

    let result = brioche::recipe::Directory::create(
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
