use std::collections::HashMap;

use assert_matches::assert_matches;
use brioche::brioche::project::{
    resolve_project, DependencyDefinition, ProjectDefinition, Version,
};

mod brioche_test;

#[tokio::test]
async fn test_resolve_simple_project() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_toml(
            "myproject/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::new(),
            },
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    assert_eq!(project.local_path, project_dir);
    assert!(project.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_resolve_project_with_repo_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_toml(
            "myproject/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::from_iter([(
                    "foo".into(),
                    DependencyDefinition::Version(Version::Any),
                )]),
            },
        )
        .await;

    context
        .write_toml(
            "brioche-repo/foo/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::new(),
            },
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    assert_eq!(project.local_path, project_dir);
    let foo_dep = &project.dependencies["foo"];
    assert_eq!(foo_dep.local_path, brioche.repo_dir.join("foo"));
    assert!(foo_dep.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_resolve_project_with_path_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let main_project_dir = context.mkdir("mainproject").await;
    context
        .write_toml(
            "mainproject/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::from_iter([(
                    "depproject".into(),
                    DependencyDefinition::Path {
                        path: "../depproject".into(),
                    },
                )]),
            },
        )
        .await;

    let dep_project_dir = context.mkdir("depproject").await;
    context
        .write_toml(
            "depproject/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::new(),
            },
        )
        .await;

    let project = resolve_project(&brioche, &main_project_dir).await?;

    assert_eq!(project.local_path, main_project_dir);
    let dep_project = &project.dependencies["depproject"];
    assert_eq!(dep_project.local_path, dep_project_dir);
    assert!(dep_project.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_resolve_complex_project() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let main_project_dir = context.mkdir("mainproject").await;
    context
        .write_toml(
            "mainproject/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::from_iter([
                    (
                        "depproject".into(),
                        DependencyDefinition::Path {
                            path: "../depproject".into(),
                        },
                    ),
                    ("foo".into(), DependencyDefinition::Version(Version::Any)),
                ]),
            },
        )
        .await;

    let dep_project_dir = context.mkdir("depproject").await;
    context
        .write_toml(
            "depproject/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::from_iter([(
                    "foo".into(),
                    DependencyDefinition::Version(Version::Any),
                )]),
            },
        )
        .await;

    context
        .write_toml(
            "brioche-repo/foo/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::from_iter([(
                    "bar".into(),
                    DependencyDefinition::Version(Version::Any),
                )]),
            },
        )
        .await;
    context
        .write_toml(
            "brioche-repo/bar/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::new(),
            },
        )
        .await;

    let project = resolve_project(&brioche, &main_project_dir).await?;
    let main_dep_project = &project.dependencies["depproject"];
    let main_foo_project = &project.dependencies["foo"];
    let main_dep_foo_project = &project.dependencies["depproject"].dependencies["foo"];
    let main_foo_bar_project = &project.dependencies["foo"].dependencies["bar"];
    let main_dep_foo_bar_project =
        &project.dependencies["depproject"].dependencies["foo"].dependencies["bar"];

    assert_eq!(main_dep_project.local_path, dep_project_dir);
    assert_eq!(main_foo_project.local_path, brioche.repo_dir.join("foo"));
    assert_eq!(
        main_dep_foo_project.local_path,
        brioche.repo_dir.join("foo")
    );
    assert_eq!(
        main_foo_bar_project.local_path,
        brioche.repo_dir.join("bar")
    );
    assert_eq!(
        main_dep_foo_bar_project.local_path,
        brioche.repo_dir.join("bar"),
    );

    Ok(())
}

#[tokio::test]
async fn test_resolve_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    // brioche.toml does not exist
    let project_dir = context.mkdir("myproject").await;

    let project = resolve_project(&brioche, &project_dir).await;

    assert_matches!(project, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_resolve_path_dep_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_toml(
            "myproject/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::from_iter([(
                    "mydep".into(),
                    DependencyDefinition::Path {
                        path: "../mydep".into(),
                    },
                )]),
            },
        )
        .await;

    // brioche.toml does not exist
    let _dep_dir = context.mkdir("mydep").await;

    let project = resolve_project(&brioche, &project_dir).await;

    assert_matches!(project, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_resolve_repo_dep_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_toml(
            "myproject/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::from_iter([(
                    "foo".into(),
                    DependencyDefinition::Version(Version::Any),
                )]),
            },
        )
        .await;

    // brioche.toml does not exist
    let _repo_foo_dir = context.mkdir("brioche-repo/foo").await;

    let project = resolve_project(&brioche, &project_dir).await;

    assert_matches!(project, Err(_));

    Ok(())
}
