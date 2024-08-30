use std::{os::unix::prelude::PermissionsExt, path::Path};

use assert_matches::assert_matches;
use brioche_core::{output::create_local_output, recipe::Artifact, Brioche};
use pretty_assertions::assert_eq;

async fn dir_is_empty(path: impl AsRef<Path>) -> bool {
    let mut entries = tokio::fs::read_dir(path).await.expect("failed to read dir");
    let first_entry = entries.next_entry().await.expect("failed to read entry");
    first_entry.is_none()
}

async fn create_output(
    brioche: &Brioche,
    output_path: &Path,
    artifact: &Artifact,
    merge: bool,
) -> anyhow::Result<()> {
    brioche_core::output::create_output(
        brioche,
        artifact,
        brioche_core::output::OutputOptions {
            output_path,
            merge,
            resource_dir: None,
            mtime: None,
            link_locals: false,
        },
    )
    .await
}

async fn create_output_with_resources(
    brioche: &Brioche,
    output_path: &Path,
    resource_dir: &Path,
    artifact: &Artifact,
    merge: bool,
) -> anyhow::Result<()> {
    brioche_core::output::create_output(
        brioche,
        artifact,
        brioche_core::output::OutputOptions {
            output_path,
            merge,
            resource_dir: Some(resource_dir),
            mtime: None,
            link_locals: false,
        },
    )
    .await
}

async fn create_output_with_links(
    brioche: &Brioche,
    output_path: &Path,
    artifact: &Artifact,
    merge: bool,
) -> anyhow::Result<()> {
    brioche_core::output::create_output(
        brioche,
        artifact,
        brioche_core::output::OutputOptions {
            output_path,
            merge,
            resource_dir: None,
            mtime: None,
            link_locals: true,
        },
    )
    .await
}

#[tokio::test]
async fn test_output_file() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact =
        brioche_test_support::file(brioche_test_support::blob(&brioche, b"hello").await, false);

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
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact =
        brioche_test_support::file(brioche_test_support::blob(&brioche, b"hello").await, true);

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

    assert_mode(
        context.path("output"),
        Mode {
            read: Some(true),
            write: Some(true),
            execute: Some(true),
        },
    )
    .await;

    Ok(())
}

#[tokio::test]
async fn test_output_symlink() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::symlink("/foo");

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
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir_empty();

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_dir());
    assert!(dir_is_empty(context.path("output")).await);

    Ok(())
}

#[tokio::test]
async fn test_output_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [
            (
                "hello",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "hi.txt",
                        brioche_test_support::file(
                            brioche_test_support::blob(&brioche, b"hello").await,
                            false,
                        ),
                    )],
                )
                .await,
            ),
            ("empty", brioche_test_support::dir_empty()),
            ("link", brioche_test_support::symlink("hello/hi.txt")),
        ],
    )
    .await;

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

    assert!(!context.path("output/brioche-resources.d").is_dir());

    Ok(())
}

#[tokio::test]
async fn test_output_merge() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [
            (
                "hello",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "hi.txt",
                        brioche_test_support::file(
                            brioche_test_support::blob(&brioche, b"hello").await,
                            false,
                        ),
                    )],
                )
                .await,
            ),
            ("empty", brioche_test_support::dir_empty()),
            ("link", brioche_test_support::symlink("hello/hi.txt")),
        ],
    )
    .await;

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
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [
            (
                "test.txt",
                brioche_test_support::file(
                    brioche_test_support::blob(&brioche, "new content").await,
                    false,
                ),
            ),
            ("link", brioche_test_support::symlink("test.txt")),
        ],
    )
    .await;

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
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [
            (
                "hello",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "hi.txt",
                        brioche_test_support::file(
                            brioche_test_support::blob(&brioche, b"hello").await,
                            false,
                        ),
                    )],
                )
                .await,
            ),
            ("empty", brioche_test_support::dir_empty()),
            ("link", brioche_test_support::symlink("hello/hi.txt")),
        ],
    )
    .await;

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
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [
            (
                "hello",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "hi.txt",
                        brioche_test_support::file(
                            brioche_test_support::blob(&brioche, b"hello").await,
                            false,
                        ),
                    )],
                )
                .await,
            ),
            ("empty", brioche_test_support::dir_empty()),
            ("link", brioche_test_support::symlink("hello/hi.txt")),
        ],
    )
    .await;

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
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [(
            "hello",
            brioche_test_support::dir(
                &brioche,
                [
                    (
                        "hi.txt",
                        brioche_test_support::file_with_resources(
                            brioche_test_support::blob(&brioche, b"hello").await,
                            false,
                            brioche_test_support::dir_value(
                                &brioche,
                                [(
                                    "hi_ref.txt",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"reference data")
                                            .await,
                                        false,
                                    ),
                                )],
                            )
                            .await,
                        ),
                    ),
                    (
                        "second.txt",
                        brioche_test_support::file_with_resources(
                            brioche_test_support::blob(&brioche, b"2").await,
                            false,
                            brioche_test_support::dir_value(
                                &brioche,
                                [(
                                    "second_refs",
                                    brioche_test_support::dir(
                                        &brioche,
                                        [(
                                            "second_ref.txt",
                                            brioche_test_support::file(
                                                brioche_test_support::blob(&brioche, b"T W O")
                                                    .await,
                                                false,
                                            ),
                                        )],
                                    )
                                    .await,
                                )],
                            )
                            .await,
                        ),
                    ),
                ],
            )
            .await,
        )],
    )
    .await;

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
        tokio::fs::read_to_string(context.path("output/brioche-resources.d/hi_ref.txt")).await?,
        "reference data",
    );
    assert_eq!(
        tokio::fs::read_to_string(
            context.path("output/brioche-resources.d/second_refs/second_ref.txt")
        )
        .await?,
        "T W O",
    );

    Ok(())
}

#[tokio::test]
async fn test_output_dir_with_resources_and_pack_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [
            (
                "hi.txt",
                brioche_test_support::file_with_resources(
                    brioche_test_support::blob(&brioche, b"hello").await,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [(
                            "hi_ref.txt",
                            brioche_test_support::file(
                                brioche_test_support::blob(&brioche, b"reference data").await,
                                false,
                            ),
                        )],
                    )
                    .await,
                ),
            ),
            (
                "brioche-resources.d",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "test.txt",
                        brioche_test_support::file(
                            brioche_test_support::blob(&brioche, b"test").await,
                            false,
                        ),
                    )],
                )
                .await,
            ),
        ],
    )
    .await;

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hi.txt")).await?,
        "hello",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-resources.d/hi_ref.txt")).await?,
        "reference data",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-resources.d/test.txt")).await?,
        "test",
    );

    Ok(())
}

#[tokio::test]
async fn test_output_dir_with_nested_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [(
            "hello",
            brioche_test_support::dir(
                &brioche,
                [(
                    "hi.txt",
                    brioche_test_support::file_with_resources(
                        brioche_test_support::blob(&brioche, b"hello").await,
                        false,
                        brioche_test_support::dir_value(
                            &brioche,
                            [(
                                "hi_ref.txt",
                                brioche_test_support::file_with_resources(
                                    brioche_test_support::blob(&brioche, b"reference data").await,
                                    false,
                                    brioche_test_support::dir_value(
                                        &brioche,
                                        [(
                                            "hi_ref_ref.txt",
                                            brioche_test_support::file(
                                                brioche_test_support::blob(
                                                    &brioche,
                                                    b"reference reference data",
                                                )
                                                .await,
                                                false,
                                            ),
                                        )],
                                    )
                                    .await,
                                ),
                            )],
                        )
                        .await,
                    ),
                )],
            )
            .await,
        )],
    )
    .await;

    create_output(&brioche, &context.path("output"), &artifact, false).await?;

    assert!(context.path("output").is_dir());

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/hello/hi.txt")).await?,
        "hello",
    );

    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-resources.d/hi_ref.txt")).await?,
        "reference data",
    );
    assert_eq!(
        tokio::fs::read_to_string(context.path("output/brioche-resources.d/hi_ref_ref.txt"))
            .await?,
        "reference reference data",
    );

    Ok(())
}

#[tokio::test]
async fn test_output_dir_with_equal_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::dir(
        &brioche,
        [(
            "hello",
            brioche_test_support::dir(
                &brioche,
                [
                    (
                        "hi.txt",
                        brioche_test_support::file_with_resources(
                            brioche_test_support::blob(&brioche, b"hello").await,
                            false,
                            brioche_test_support::dir_value(
                                &brioche,
                                [(
                                    "same.txt",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"a").await,
                                        false,
                                    ),
                                )],
                            )
                            .await,
                        ),
                    ),
                    (
                        "second.txt",
                        brioche_test_support::file_with_resources(
                            brioche_test_support::blob(&brioche, b"2").await,
                            false,
                            brioche_test_support::dir_value(
                                &brioche,
                                [(
                                    "same.txt",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"a").await,
                                        false,
                                    ),
                                )],
                            )
                            .await,
                        ),
                    ),
                ],
            )
            .await,
        )],
    )
    .await;

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
        tokio::fs::read_to_string(context.path("output/brioche-resources.d/same.txt")).await?,
        "a",
    );

    Ok(())
}

#[tokio::test]
async fn test_output_top_level_file_with_resources_fails() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::file_with_resources(
        brioche_test_support::blob(&brioche, b"hello").await,
        false,
        brioche_test_support::dir_value(
            &brioche,
            [(
                "same.txt",
                brioche_test_support::file(brioche_test_support::blob(&brioche, b"a").await, false),
            )],
        )
        .await,
    );

    let result = create_output(&brioche, &context.path("output"), &artifact, false).await;

    assert_matches!(result, Err(_));
    assert!(!context.path("output").exists());

    Ok(())
}

#[tokio::test]
async fn test_output_top_level_file_with_parallel_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let artifact = brioche_test_support::file_with_resources(
        brioche_test_support::blob(&brioche, b"hello").await,
        false,
        brioche_test_support::dir_value(
            &brioche,
            [(
                "resource.txt",
                brioche_test_support::file(brioche_test_support::blob(&brioche, b"a").await, false),
            )],
        )
        .await,
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

#[tokio::test]
async fn test_output_with_links() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let hello_blob = brioche_test_support::blob(&brioche, b"hello").await;
    let hello_blob_path = brioche_core::blob::local_blob_path(&brioche, hello_blob);

    let hello = brioche_test_support::file(hello_blob, false);
    let hello_exe = brioche_test_support::file(hello_blob, true);

    let hello_with_resource = brioche_test_support::file_with_resources(
        hello_blob,
        false,
        brioche_test_support::dir_value(&brioche, [("resource.txt", hello.clone())]).await,
    );
    let hello_exe_with_resource = brioche_test_support::file_with_resources(
        hello_blob,
        true,
        brioche_test_support::dir_value(&brioche, [("resource.txt", hello.clone())]).await,
    );

    let artifact = brioche_test_support::dir(
        &brioche,
        [
            ("hello.txt", hello.clone()),
            ("hello2.txt", hello.clone()),
            ("hello_res.txt", hello_with_resource.clone()),
            ("hello_res2.txt", hello_with_resource.clone()),
            ("hello_exe", hello_exe.clone()),
            ("hello_exe2", hello_exe.clone()),
            ("hello_exe_res", hello_exe_with_resource.clone()),
            ("hello_exe_res2", hello_exe_with_resource.clone()),
            (
                "hi.txt",
                brioche_test_support::file(
                    brioche_test_support::blob(&brioche, b"hi").await,
                    false,
                ),
            ),
        ],
    )
    .await;

    create_output_with_links(&brioche, &context.path("output"), &artifact, false).await?;

    let hello_local = create_local_output(&brioche, &hello).await?;
    let hello_exe_local = create_local_output(&brioche, &hello_exe).await?;
    let hello_with_resource_local = create_local_output(&brioche, &hello_with_resource).await?;

    assert!(context.path("output").is_dir());

    assert_linked(context.path("output/hello.txt"), &hello_local.path).await;
    assert_linked(context.path("output/hello2.txt"), &hello_local.path).await;

    assert_linked(
        context.path("output/hello_res.txt"),
        &hello_with_resource_local.path,
    )
    .await;
    assert_linked(
        context.path("output/hello_res2.txt"),
        &hello_with_resource_local.path,
    )
    .await;

    assert_linked(context.path("output/hello_exe"), &hello_exe_local.path).await;
    assert_linked(context.path("output/hello_exe2"), &hello_exe_local.path).await;

    assert_linked(context.path("output/hello_exe_res"), &hello_exe_local.path).await;
    assert_linked(context.path("output/hello_exe_res2"), &hello_exe_local.path).await;

    assert_linked(
        context.path("output/hello.txt"),
        context.path("output/hello2.txt"),
    )
    .await;
    assert_linked(
        context.path("output/hello.txt"),
        context.path("output/hello_res.txt"),
    )
    .await;
    assert_linked(
        context.path("output/hello.txt"),
        context.path("output/hello_res2.txt"),
    )
    .await;
    assert_not_linked(
        context.path("output/hello.txt"),
        context.path("output/hello_exe"),
    )
    .await;
    assert_not_linked(
        context.path("output/hello.txt"),
        context.path("output/hello_exe2"),
    )
    .await;
    assert_not_linked(
        context.path("output/hello.txt"),
        context.path("output/hello_exe_res"),
    )
    .await;
    assert_not_linked(
        context.path("output/hello.txt"),
        context.path("output/hello_exe_res2"),
    )
    .await;
    assert_not_linked(
        context.path("output/hello.txt"),
        context.path("output/hi.txt"),
    )
    .await;

    assert_linked(
        context.path("output/hello_exe"),
        context.path("output/hello_exe2"),
    )
    .await;
    assert_linked(
        context.path("output/hello_exe"),
        context.path("output/hello_exe_res"),
    )
    .await;
    assert_linked(
        context.path("output/hello_exe"),
        context.path("output/hello_exe_res2"),
    )
    .await;
    assert_not_linked(
        context.path("output/hello_exe"),
        context.path("output/hi.txt"),
    )
    .await;

    assert_linked(context.path("output/hello.txt"), &hello_blob_path).await;
    assert_not_linked(context.path("output/hello_exe"), &hello_blob_path).await;

    assert_mode(
        context.path("output"),
        Mode {
            read: Some(true),
            write: Some(true),
            execute: Some(true),
        },
    )
    .await;
    assert_mode(
        context.path("output/hello.txt"),
        Mode {
            read: Some(true),
            write: Some(false),
            execute: Some(false),
        },
    )
    .await;
    assert_mode(
        context.path("output/hello2.txt"),
        Mode {
            read: Some(true),
            write: Some(false),
            execute: Some(false),
        },
    )
    .await;
    assert_mode(
        context.path("output/hello_res.txt"),
        Mode {
            read: Some(true),
            write: Some(false),
            execute: Some(false),
        },
    )
    .await;
    assert_mode(
        context.path("output/hello_res2.txt"),
        Mode {
            read: Some(true),
            write: Some(false),
            execute: Some(false),
        },
    )
    .await;
    assert_mode(
        context.path("output/hello_exe"),
        Mode {
            read: Some(true),
            write: Some(false),
            execute: Some(true),
        },
    )
    .await;
    assert_mode(
        context.path("output/hello_exe2"),
        Mode {
            read: Some(true),
            write: Some(false),
            execute: Some(true),
        },
    )
    .await;
    assert_mode(
        context.path("output/hello_exe_res"),
        Mode {
            read: Some(true),
            write: Some(false),
            execute: Some(true),
        },
    )
    .await;
    assert_mode(
        context.path("output/hello_exe_res2"),
        Mode {
            read: Some(true),
            write: Some(false),
            execute: Some(true),
        },
    )
    .await;

    assert_mtime_is_brioche_epoch(context.path("output/hello.txt")).await;
    assert_mtime_is_brioche_epoch(context.path("output/hello2.txt")).await;
    assert_mtime_is_brioche_epoch(context.path("output/hello_res.txt")).await;
    assert_mtime_is_brioche_epoch(context.path("output/hello_res2.txt")).await;
    assert_mtime_is_brioche_epoch(context.path("output/hello_exe")).await;
    assert_mtime_is_brioche_epoch(context.path("output/hello_exe2")).await;
    assert_mtime_is_brioche_epoch(context.path("output/hello_exe_res")).await;
    assert_mtime_is_brioche_epoch(context.path("output/hello_exe_res2")).await;

    Ok(())
}

async fn assert_mtime_is_brioche_epoch(path: impl AsRef<Path>) {
    let path = path.as_ref();
    let metadata = tokio::fs::metadata(path)
        .await
        .expect("failed to get metadata");
    let mtime = metadata.modified().expect("failed to get mtime");
    assert_eq!(
        mtime
            .duration_since(brioche_core::fs_utils::brioche_epoch())
            .expect("mtime is before epoch"),
        std::time::Duration::ZERO,
    );
}

struct Mode {
    read: Option<bool>,
    write: Option<bool>,
    execute: Option<bool>,
}

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        async fn assert_linked(a: impl AsRef<Path>, b: impl AsRef<Path>) {
            use std::os::unix::fs::MetadataExt as _;
            let a = a.as_ref();
            let b = b.as_ref();

            let a_metadata = tokio::fs::metadata(a).await.expect("failed to get metadata");
            let b_metadata = tokio::fs::metadata(b).await.expect("failed to get metadata");

            assert_eq!(a_metadata.ino(), b_metadata.ino(), "expected {} to be linked to {}", a.display(), b.display());
        }

        async fn assert_not_linked(a: impl AsRef<Path>, b: impl AsRef<Path>) {
            use std::os::unix::fs::MetadataExt as _;
            let a = a.as_ref();
            let b = b.as_ref();

            let a_metadata = tokio::fs::metadata(a).await.expect("failed to get metadata");
            let b_metadata = tokio::fs::metadata(b).await.expect("failed to get metadata");

            assert_ne!(a_metadata.ino(), b_metadata.ino(), "expected {} not to be linked to {}", a.display(), b.display());
        }

        async fn assert_mode(path: impl AsRef<Path>, mode: Mode) {
            use std::os::unix::fs::PermissionsExt as _;

            let path = path.as_ref();

            let metadata = tokio::fs::metadata(path).await.expect("failed to get metadata");
            let permissions = metadata.permissions();

            let unix_mode = permissions.mode();
            if let Some(read_expected) = mode.read {
                let read_actual = unix_mode & 0o400 == 0o400;
                assert_eq!(read_expected, read_actual, "expected read permission for {} to be {read_expected}", path.display());
            }

            if let Some(write_expected) = mode.write {
                let write_actual = unix_mode & 0o200 == 0o200;
                assert_eq!(write_expected, write_actual, "expected write permission for {} to be {write_expected}", path.display());
            }

            if let Some(execute_expected) = mode.execute {
                let execute_actual = unix_mode & 0o100 == 0o100;
                assert_eq!(execute_expected, execute_actual, "expected execute permission for {} to be {execute_expected}", path.display());
            }
        }
    }
}
