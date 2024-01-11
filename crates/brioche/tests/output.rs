use std::{os::unix::prelude::PermissionsExt, path::Path};

use assert_matches::assert_matches;
use brioche::brioche::{artifact::CompleteArtifact, Brioche};
use pretty_assertions::assert_eq;

mod brioche_test;

async fn dir_is_empty(path: impl AsRef<Path>) -> bool {
    let mut entries = tokio::fs::read_dir(path).await.expect("failed to read dir");
    let first_entry = entries.next_entry().await.expect("failed to read entry");
    first_entry.is_none()
}

async fn create_output(
    brioche: &Brioche,
    output_path: &Path,
    artifact: &CompleteArtifact,
    merge: bool,
) -> anyhow::Result<()> {
    brioche::brioche::output::create_output(
        brioche,
        artifact,
        brioche::brioche::output::OutputOptions {
            output_path,
            merge,
            resources_dir: None,
        },
    )
    .await
}

async fn create_output_with_resources(
    brioche: &Brioche,
    output_path: &Path,
    resources_dir: &Path,
    artifact: &CompleteArtifact,
    merge: bool,
) -> anyhow::Result<()> {
    brioche::brioche::output::create_output(
        brioche,
        artifact,
        brioche::brioche::output::OutputOptions {
            output_path,
            merge,
            resources_dir: Some(resources_dir),
        },
    )
    .await
}

#[tokio::test]
async fn test_output_file() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::file(brioche_test::blob(&brioche, b"hello").await, false);

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    let contents = tokio::fs::read_to_string(&context.path("output"))
        .await
        .expect("failed to read output file");
    assert_eq!(contents, "hello");

    let permissions = tokio::fs::metadata(&context.path("output"))
        .await
        .expect("failed to get metadata")
        .permissions();
    assert_eq!(permissions.mode() & 0o777, 0o644);

    Ok(())
}

#[tokio::test]
async fn test_output_executable_file() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::file(brioche_test::blob(&brioche, b"hello").await, true);

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    let contents = tokio::fs::read_to_string(&context.path("output"))
        .await
        .expect("failed to read output file");
    assert_eq!(contents, "hello");

    let permissions = tokio::fs::metadata(&context.path("output"))
        .await
        .expect("failed to get metadata")
        .permissions();
    assert_eq!(permissions.mode() & 0o777, 0o755);

    Ok(())
}

#[tokio::test]
async fn test_output_symlink() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::symlink("/foo");

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_symlink());
    let target = tokio::fs::read_link(&context.path("output"))
        .await
        .expect("failed to read symlink target");
    assert_eq!(target, Path::new("/foo"));

    Ok(())
}

#[tokio::test]
async fn test_output_empty_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir_empty();

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_dir());
    assert!(dir_is_empty(context.path("output")).await);

    Ok(())
}

#[tokio::test]
async fn test_output_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([
        (
            "hello",
            brioche_test::dir([(
                "hi.txt",
                brioche_test::file(brioche_test::blob(&brioche, b"hello").await, false),
            )]),
        ),
        ("empty", brioche_test::dir_empty()),
        ("link", brioche_test::symlink("hello/hi.txt")),
    ]);

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hello/hi.txt")).await?,
        "hello",
    );
    assert_eq!(
        tokio::fs::read_link(context.path("output/link")).await?,
        std::path::PathBuf::from("hello/hi.txt"),
    );
    assert!(dir_is_empty(context.path("output/empty")).await);

    assert!(!context.path("output/brioche-pack.d").is_dir());

    Ok(())
}

#[tokio::test]
async fn test_output_merge() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([
        (
            "hello",
            brioche_test::dir([(
                "hi.txt",
                brioche_test::file(brioche_test::blob(&brioche, b"hello").await, false),
            )]),
        ),
        ("empty", brioche_test::dir_empty()),
        ("link", brioche_test::symlink("hello/hi.txt")),
    ]);

    context.write_file("output/foo.txt", "foo").await;

    create_output(&brioche, &context.path("output"), &artifact, true).await?;

    assert!(context.path("output").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/foo.txt")).await?,
        "foo",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hello/hi.txt")).await?,
        "hello",
    );
    assert_eq!(
        tokio::fs::read_link(context.path("output/link")).await?,
        std::path::PathBuf::from("hello/hi.txt"),
    );
    assert!(dir_is_empty(context.path("output/empty")).await);

    Ok(())
}

#[tokio::test]
async fn test_output_merge_replace() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([
        (
            "test.txt",
            brioche_test::file(brioche_test::blob(&brioche, "new content").await, false),
        ),
        ("link", brioche_test::symlink("test.txt")),
    ]);

    context.write_file("output/test.txt", "old content").await;
    context.write_file("output/test2.txt", "unchanged").await;
    context.write_symlink("test2.txt", "output/link").await;

    create_output(&brioche, &context.path("output"), &artifact, true).await?;

    assert!(context.path("output").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/test.txt")).await?,
        "new content",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/test2.txt")).await?,
        "unchanged",
    );
    assert_eq!(
        tokio::fs::read_link(context.path("output/link")).await?,
        std::path::PathBuf::from("test.txt"),
    );

    Ok(())
}

#[tokio::test]
async fn test_output_conflict_no_merge() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([
        (
            "hello",
            brioche_test::dir([(
                "hi.txt",
                brioche_test::file(brioche_test::blob(&brioche, b"hello").await, false),
            )]),
        ),
        ("empty", brioche_test::dir_empty()),
        ("link", brioche_test::symlink("hello/hi.txt")),
    ]);

    context.write_file("output1", "foo").await;
    context.write_symlink("/foo", "output2").await;
    context.mkdir("output3").await;

    // Each of these fails because the output path already exists

    let output_1_result = create_output(&brioche, &context.path("output1"), &artifact, false).await;
    assert_matches!(output_1_result, Err(_));
    assert!(context.path("output1").is_file());

    let output_2_result = create_output(&brioche, &context.path("output2"), &artifact, false).await;
    assert_matches!(output_2_result, Err(_));
    assert!(context.path("output2").is_symlink());

    let output_3_result = create_output(&brioche, &context.path("output3"), &artifact, false).await;
    assert_matches!(output_3_result, Err(_));
    assert!(dir_is_empty(context.path("output3")).await);

    Ok(())
}

#[tokio::test]
async fn test_output_conflict_merge() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([
        (
            "hello",
            brioche_test::dir([(
                "hi.txt",
                brioche_test::file(brioche_test::blob(&brioche, b"hello").await, false),
            )]),
        ),
        ("empty", brioche_test::dir_empty()),
        ("link", brioche_test::symlink("hello/hi.txt")),
    ]);

    context.write_file("output1", "foo").await;
    context.write_symlink("/foo", "output2").await;
    context.mkdir("output3").await;

    // Each of these fails because the output path already exists and is not a directory

    let output_1_result = create_output(&brioche, &context.path("output1"), &artifact, true).await;
    assert_matches!(output_1_result, Err(_));
    assert!(context.path("output1").is_file());

    let output_2_result = create_output(&brioche, &context.path("output2"), &artifact, true).await;
    assert_matches!(output_2_result, Err(_));
    assert!(context.path("output2").is_symlink());

    Ok(())
}

#[tokio::test]
async fn test_output_dir_with_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([(
        "hello",
        brioche_test::dir([
            (
                "hi.txt",
                brioche_test::file_with_resources(
                    brioche_test::blob(&brioche, b"hello").await,
                    false,
                    brioche_test::dir_value([(
                        "hi_ref.txt",
                        brioche_test::file(
                            brioche_test::blob(&brioche, b"reference data").await,
                            false,
                        ),
                    )]),
                ),
            ),
            (
                "second.txt",
                brioche_test::file_with_resources(
                    brioche_test::blob(&brioche, b"2").await,
                    false,
                    brioche_test::dir_value([(
                        "second_refs",
                        brioche_test::dir([(
                            "second_ref.txt",
                            brioche_test::file(brioche_test::blob(&brioche, b"T W O").await, false),
                        )]),
                    )]),
                ),
            ),
        ]),
    )]);

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hello/hi.txt")).await?,
        "hello",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hello/second.txt")).await?,
        "2",
    );

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-pack.d/hi_ref.txt")).await?,
        "reference data",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-pack.d/second_refs/second_ref.txt"))
            .await?,
        "T W O",
    );

    Ok(())
}

#[tokio::test]
async fn test_output_dir_with_resources_and_pack_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([
        (
            "hi.txt",
            brioche_test::file_with_resources(
                brioche_test::blob(&brioche, b"hello").await,
                false,
                brioche_test::dir_value([(
                    "hi_ref.txt",
                    brioche_test::file(
                        brioche_test::blob(&brioche, b"reference data").await,
                        false,
                    ),
                )]),
            ),
        ),
        (
            "brioche-pack.d",
            brioche_test::dir([(
                "test.txt",
                brioche_test::file(brioche_test::blob(&brioche, b"test").await, false),
            )]),
        ),
    ]);

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hi.txt")).await?,
        "hello",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-pack.d/hi_ref.txt")).await?,
        "reference data",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-pack.d/test.txt")).await?,
        "test",
    );

    Ok(())
}

#[tokio::test]
async fn test_output_dir_with_nested_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([(
        "hello",
        brioche_test::dir([(
            "hi.txt",
            brioche_test::file_with_resources(
                brioche_test::blob(&brioche, b"hello").await,
                false,
                brioche_test::dir_value([(
                    "hi_ref.txt",
                    brioche_test::file_with_resources(
                        brioche_test::blob(&brioche, b"reference data").await,
                        false,
                        brioche_test::dir_value([(
                            "hi_ref_ref.txt",
                            brioche_test::file(
                                brioche_test::blob(&brioche, b"reference reference data").await,
                                false,
                            ),
                        )]),
                    ),
                )]),
            ),
        )]),
    )]);

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hello/hi.txt")).await?,
        "hello",
    );

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-pack.d/hi_ref.txt")).await?,
        "reference data",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-pack.d/hi_ref_ref.txt")).await?,
        "reference reference data",
    );

    Ok(())
}

#[tokio::test]
async fn test_output_dir_with_equal_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::dir([(
        "hello",
        brioche_test::dir([
            (
                "hi.txt",
                brioche_test::file_with_resources(
                    brioche_test::blob(&brioche, b"hello").await,
                    false,
                    brioche_test::dir_value([(
                        "same.txt",
                        brioche_test::file(brioche_test::blob(&brioche, b"a").await, false),
                    )]),
                ),
            ),
            (
                "second.txt",
                brioche_test::file_with_resources(
                    brioche_test::blob(&brioche, b"2").await,
                    false,
                    brioche_test::dir_value([(
                        "same.txt",
                        brioche_test::file(brioche_test::blob(&brioche, b"a").await, false),
                    )]),
                ),
            ),
        ]),
    )]);

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hello/hi.txt")).await?,
        "hello",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hello/second.txt")).await?,
        "2",
    );

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-pack.d/same.txt")).await?,
        "a",
    );

    Ok(())
}

#[tokio::test]
async fn test_output_top_level_file_with_resources_fails() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::file_with_resources(
        brioche_test::blob(&brioche, b"hello").await,
        false,
        brioche_test::dir_value([(
            "same.txt",
            brioche_test::file(brioche_test::blob(&brioche, b"a").await, false),
        )]),
    );

    let result = create_output(&brioche, &context.path("output"), &artifact, false).await;

    assert_matches!(result, Err(_));
    assert!(!context.path("output").exists());

    Ok(())
}

#[tokio::test]
async fn test_output_top_level_file_with_parallel_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let artifact = brioche_test::file_with_resources(
        brioche_test::blob(&brioche, b"hello").await,
        false,
        brioche_test::dir_value([(
            "resource.txt",
            brioche_test::file(brioche_test::blob(&brioche, b"a").await, false),
        )]),
    );

    create_output_with_resources(
        &brioche,
        &context.path("output"),
        &context.path("resources"),
        &artifact,
        false,
    )
    .await?;

    assert!(context.path("output").is_file());
    assert!(context.path("resources").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output")).await?,
        "hello",
    );

    assert_eq!(
        tokio::fs::read_to_string(context.path("resources/resource.txt")).await?,
        "a",
    );

    Ok(())
}
