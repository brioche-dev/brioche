use std::{
    os::unix::prelude::PermissionsExt as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use brioche_core::{
    recipe::{Artifact, Meta},
    Brioche,
};
use pretty_assertions::assert_eq;

async fn create_input(
    brioche: &Brioche,
    input_path: &Path,
    remove_input: bool,
) -> anyhow::Result<Artifact> {
    let artifact = brioche_core::input::create_input(
        brioche,
        brioche_core::input::InputOptions {
            input_path,
            remove_input,
            resource_dir: None,
            input_resource_dirs: &[],
            saved_paths: &mut Default::default(),
            meta: &Arc::new(Meta::default()),
        },
    )
    .await?;

    Ok(artifact.value)
}

async fn create_input_with_resources(
    brioche: &Brioche,
    input_path: &Path,
    resource_dir: Option<&Path>,
    input_resource_dirs: &[PathBuf],
    remove_input: bool,
) -> anyhow::Result<Artifact> {
    let artifact = brioche_core::input::create_input(
        brioche,
        brioche_core::input::InputOptions {
            input_path,
            remove_input,
            resource_dir,
            input_resource_dirs,
            saved_paths: &mut Default::default(),
            meta: &Arc::new(Meta::default()),
        },
    )
    .await?;
    Ok(artifact.value)
}

#[tokio::test]
async fn test_input_file() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let file_path = context.write_file("hello.txt", b"hello").await;

    let artifact = create_input(&brioche, &file_path, false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::file(brioche_test_support::blob(&brioche, b"hello").await, false)
    );
    assert!(file_path.is_file());

    Ok(())
}

#[tokio::test]
async fn test_input_executable_file() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let file_path = context.write_file("hello.txt", b"hello").await;
    let mut permissions = tokio::fs::metadata(&file_path)
        .await
        .expect("failed to get metadata")
        .permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(&file_path, permissions)
        .await
        .expect("failed to set permissions");

    let artifact = create_input(&brioche, &file_path, false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::file(brioche_test_support::blob(&brioche, b"hello").await, true)
    );
    assert!(file_path.is_file());

    Ok(())
}

#[tokio::test]
async fn test_input_symlink() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let symlink_path = context.write_symlink("/foo", "foo").await;

    let artifact = create_input(&brioche, &symlink_path, false).await?;

    assert_eq!(artifact, brioche_test_support::symlink("/foo"));
    assert!(symlink_path.is_symlink());

    Ok(())
}

#[tokio::test]
async fn test_input_empty_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;

    let artifact = create_input(&brioche, &dir_path, false).await?;

    assert_eq!(artifact, brioche_test_support::dir_empty());
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    context.write_file("test/hello/hi.txt", b"hello").await;
    context.mkdir("test/empty").await;
    context.write_symlink("hello/hi.txt", "test/link").await;

    let artifact = create_input(&brioche, &dir_path, false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
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
                                false
                            )
                        ),]
                    )
                    .await
                ),
                ("empty", brioche_test_support::dir_empty()),
                ("link", brioche_test_support::symlink("hello/hi.txt"))
            ]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_remove_original() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    context.write_file("test/hello/hi.txt", b"hello").await;
    context.mkdir("test/empty").await;
    context.write_symlink("hello/hi.txt", "test/link").await;

    let artifact = create_input(&brioche, &dir_path, true).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
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
                                false
                            )
                        ),]
                    )
                    .await
                ),
                ("empty", brioche_test_support::dir_empty()),
                ("link", brioche_test_support::symlink("hello/hi.txt"))
            ]
        )
        .await
    );
    assert!(!dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir_treat_pack_normally() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;

    let mut packed_file = b"test".to_vec();
    brioche_pack::inject_pack(
        &mut packed_file,
        &brioche_pack::Pack::LdLinux {
            program: b"test".into(),
            interpreter: b"test".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    context.write_file("test/hi", &packed_file).await;
    context
        .write_file("test/brioche-resources.d/test", b"test")
        .await;

    let artifact = create_input(&brioche, &dir_path, false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
            &brioche,
            [
                (
                    "hi",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, &packed_file).await,
                        false
                    )
                ),
                (
                    "brioche-resources.d",
                    brioche_test_support::dir(
                        &brioche,
                        [(
                            "test",
                            brioche_test_support::file(
                                brioche_test_support::blob(&brioche, b"test").await,
                                false
                            )
                        ),]
                    )
                    .await
                ),
            ]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir_use_resource_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    let resource_dir = context.mkdir("resources").await;

    let mut packed_file = b"test".to_vec();
    brioche_pack::inject_pack(
        &mut packed_file,
        &brioche_pack::Pack::LdLinux {
            program: b"test".into(),
            interpreter: b"test".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    context.write_file("test/hi", &packed_file).await;
    context.write_file("resources/test", b"test").await;

    let artifact =
        create_input_with_resources(&brioche, &dir_path, Some(&resource_dir), &[], false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
            &brioche,
            [(
                "hi",
                brioche_test_support::file_with_resources(
                    brioche_test_support::blob(&brioche, &packed_file).await,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [(
                            "test",
                            brioche_test_support::file(
                                brioche_test_support::blob(&brioche, b"test").await,
                                false
                            ),
                        )]
                    )
                    .await,
                )
            ),]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir_use_input_resource_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    let resource_dir = context.mkdir("resources").await;
    let input_resource_dir = context.mkdir("input_resources").await;

    let mut packed_file = b"test".to_vec();
    brioche_pack::inject_pack(
        &mut packed_file,
        &brioche_pack::Pack::LdLinux {
            program: b"test".into(),
            interpreter: b"test".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    context.write_file("test/hi", &packed_file).await;
    context.write_file("input_resources/test", b"test").await;

    let artifact = create_input_with_resources(
        &brioche,
        &dir_path,
        Some(&resource_dir),
        &[input_resource_dir],
        false,
    )
    .await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
            &brioche,
            [(
                "hi",
                brioche_test_support::file_with_resources(
                    brioche_test_support::blob(&brioche, &packed_file).await,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [(
                            "test",
                            brioche_test_support::file(
                                brioche_test_support::blob(&brioche, b"test").await,
                                false
                            ),
                        )]
                    )
                    .await,
                )
            ),]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir_use_only_input_resource_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    let resource_dir = context.mkdir("resources").await;

    let mut packed_file = b"test".to_vec();
    brioche_pack::inject_pack(
        &mut packed_file,
        &brioche_pack::Pack::LdLinux {
            program: b"test".into(),
            interpreter: b"test".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    context.write_file("test/hi", &packed_file).await;
    context.write_file("resources/test", b"test").await;

    let artifact =
        create_input_with_resources(&brioche, &dir_path, None, &[resource_dir], false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
            &brioche,
            [(
                "hi",
                brioche_test_support::file_with_resources(
                    brioche_test_support::blob(&brioche, &packed_file).await,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [(
                            "test",
                            brioche_test_support::file(
                                brioche_test_support::blob(&brioche, b"test").await,
                                false
                            ),
                        )]
                    )
                    .await,
                )
            ),]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir_with_symlink_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    let resource_dir = context.mkdir("resources").await;

    let mut packed_file = b"test".to_vec();
    brioche_pack::inject_pack(
        &mut packed_file,
        &brioche_pack::Pack::LdLinux {
            program: b"test".into(),
            interpreter: b"test".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    context.write_file("test/hi", &packed_file).await;
    context.write_symlink("test_target", "resources/test").await;
    context.write_file("resources/test_target", b"test").await;

    let artifact =
        create_input_with_resources(&brioche, &dir_path, Some(&resource_dir), &[], false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
            &brioche,
            [(
                "hi",
                brioche_test_support::file_with_resources(
                    brioche_test_support::blob(&brioche, &packed_file).await,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            ("test", brioche_test_support::symlink(b"test_target")),
                            (
                                "test_target",
                                brioche_test_support::file(
                                    brioche_test_support::blob(&brioche, b"test").await,
                                    false
                                )
                            ),
                        ]
                    )
                    .await,
                )
            ),]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir_broken_symlink() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    let resource_dir = context.mkdir("resources").await;

    let mut packed_file = b"test".to_vec();
    brioche_pack::inject_pack(
        &mut packed_file,
        &brioche_pack::Pack::LdLinux {
            program: b"test".into(),
            interpreter: b"test".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    context.write_file("test/hi", &packed_file).await;
    context.write_symlink("test_target", "resources/test").await;

    let artifact =
        create_input_with_resources(&brioche, &dir_path, Some(&resource_dir), &[], false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
            &brioche,
            [(
                "hi",
                brioche_test_support::file_with_resources(
                    brioche_test_support::blob(&brioche, &packed_file).await,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [("test", brioche_test_support::symlink(b"test_target"))]
                    )
                    .await,
                )
            ),]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir_with_dir_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    let resource_dir = context.mkdir("resources").await;

    let mut packed_file = b"test".to_vec();
    brioche_pack::inject_pack(
        &mut packed_file,
        &brioche_pack::Pack::LdLinux {
            program: b"test".into(),
            interpreter: b"test".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    context.write_file("test/hi", &packed_file).await;
    context.write_file("resources/test/hi", b"test").await;
    context
        .write_symlink("../test_target", "resources/test/target")
        .await;
    context.write_file("resources/test_target", b"test").await;

    let artifact =
        create_input_with_resources(&brioche, &dir_path, Some(&resource_dir), &[], false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
            &brioche,
            [(
                "hi",
                brioche_test_support::file_with_resources(
                    brioche_test_support::blob(&brioche, &packed_file).await,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [
                            (
                                "test",
                                brioche_test_support::dir(
                                    &brioche,
                                    [
                                        (
                                            "hi",
                                            brioche_test_support::file(
                                                brioche_test_support::blob(&brioche, b"test").await,
                                                false
                                            )
                                        ),
                                        (
                                            "target",
                                            brioche_test_support::symlink(b"../test_target")
                                        ),
                                    ]
                                )
                                .await
                            ),
                            (
                                "test_target",
                                brioche_test_support::file(
                                    brioche_test_support::blob(&brioche, b"test").await,
                                    false
                                )
                            ),
                        ]
                    )
                    .await,
                )
            ),]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}

#[tokio::test]
async fn test_input_dir_omits_unused_resources() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let dir_path = context.mkdir("test").await;
    let resource_dir = context.mkdir("resources").await;

    let mut packed_file = b"test".to_vec();
    brioche_pack::inject_pack(
        &mut packed_file,
        &brioche_pack::Pack::LdLinux {
            program: b"test".into(),
            interpreter: b"test".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    context.write_file("test/hi", &packed_file).await;
    context.write_file("resources/test", "hello").await;

    // Not referenced by any pack
    context.write_file("resources/unused.txt", "other").await;

    let artifact =
        create_input_with_resources(&brioche, &dir_path, Some(&resource_dir), &[], false).await?;

    assert_eq!(
        artifact,
        brioche_test_support::dir(
            &brioche,
            [(
                "hi",
                brioche_test_support::file_with_resources(
                    brioche_test_support::blob(&brioche, &packed_file).await,
                    false,
                    brioche_test_support::dir_value(
                        &brioche,
                        [(
                            "test",
                            brioche_test_support::file(
                                brioche_test_support::blob(&brioche, b"hello").await,
                                false
                            ),
                        )]
                    )
                    .await,
                )
            ),]
        )
        .await
    );
    assert!(dir_path.is_dir());

    Ok(())
}
