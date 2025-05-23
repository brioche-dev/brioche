use std::sync::Arc;

use assert_matches::assert_matches;
use brioche_core::{
    Brioche,
    project::{DependencyRef, ProjectLocking, ProjectValidation},
};
use brioche_test_support::TestContext;

#[tokio::test]
async fn test_project_load_simple() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let project = projects.project(project_hash).unwrap();
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );
    assert!(project.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_project_load_simple_no_definition() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context.write_file("myproject/project.bri", r#""#).await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let project = projects.project(project_hash).unwrap();
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );
    assert!(project.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_workspace_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./foo".parse()?],
            },
        )
        .await;

    let workspace_foo_dir = context.mkdir("myworkspace/foo").await;
    context
        .write_file(
            "myworkspace/foo/project.bri",
            r#"
                // Workspace foo
            "#,
        )
        .await;

    let (registry_foo_hash, registry_foo_dir) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    // Registry foo
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("foo", "latest", registry_foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myworkspace/myproject").await;
    context
        .write_file(
            "myworkspace/myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    // "foo" from the workspace should take precedence over the repo dep
    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&workspace_foo_dir)
    );
    assert!(
        !projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&registry_foo_dir)
    );
    assert_eq!(foo_dep.dependencies.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_workspace_dep_implied() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./foo".parse()?],
            },
        )
        .await;

    let workspace_foo_dir = context.mkdir("myworkspace/foo").await;
    context
        .write_file(
            "myworkspace/foo/project.bri",
            r#"
                // Workspace foo
            "#,
        )
        .await;

    let (registry_foo_hash, registry_foo_dir) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    // Registry foo
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("foo", "latest", registry_foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myworkspace/myproject").await;
    context
        .write_file(
            "myworkspace/myproject/project.bri",
            r#"
                import "foo";
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    // "foo" from the workspace should take precedence over the repo dep
    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&workspace_foo_dir)
    );
    assert!(
        !projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&registry_foo_dir)
    );
    assert!(foo_dep.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_path_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let main_project_dir = context.mkdir("mainproject").await;
    context
        .write_file(
            "mainproject/project.bri",
            r#"
                import "depproject";
                export const project = {
                    dependencies: {
                        depproject: {
                            path: "../depproject",
                        },
                    },
                };
            "#,
        )
        .await;

    let dep_project_dir = context.mkdir("depproject").await;
    context
        .write_file(
            "depproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &main_project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&main_project_dir)
    );

    let dep_project_hash = projects.project_dependencies(project_hash).unwrap()["depproject"];
    let dep_project = projects.project(dep_project_hash).unwrap();
    assert!(
        projects
            .local_paths(dep_project_hash)
            .unwrap()
            .contains(&dep_project_dir)
    );
    assert!(dep_project.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_local_registry_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {};
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_local_registry_dep_implied() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {};
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import "foo";
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_local_registry_dep_implied_nested() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    // foo
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import "./module.bri";
            "#,
        )
        .await;
    context
        .write_file(
            "myproject/module.bri",
            r#"
                import "foo";
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_local_registry_dep_imported() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {};
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import "foo";

                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_registry_dep() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {};
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    let foo_path = brioche
        .data_dir
        .join("projects")
        .join(foo_dep_hash.to_string())
        .canonicalize()
        .unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_registry_dep_with_brioche_include() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(path.join("fizz"), "fizz!").await.unwrap();
            tokio::fs::create_dir_all(path.join("buzz/hello"))
                .await
                .unwrap();
            tokio::fs::write(path.join("buzz/hello/world.txt"), "buzz!")
                .await
                .unwrap();
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {
                        name: "foo",
                    };

                    export const foo = Brioche.includeFile("fizz");
                    export const bar = Brioche.includeDirectory("buzz");
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    let foo_path = brioche
        .data_dir
        .join("projects")
        .join(foo_dep_hash.to_string())
        .canonicalize()
        .unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    mock_foo_latest.assert_async().await;

    let fizz_path = foo_path.join("fizz");
    let buzz_hello_world_path = foo_path.join("buzz/hello/world.txt");

    assert!(tokio::fs::try_exists(&fizz_path).await?);
    assert!(tokio::fs::try_exists(&buzz_hello_world_path).await?);

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_registry_dep_with_brioche_glob() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::create_dir_all(path.join("fizz")).await.unwrap();
            tokio::fs::write(path.join("fizz/hello.md"), "fizz!")
                .await
                .unwrap();
            tokio::fs::create_dir_all(path.join("buzz")).await.unwrap();
            tokio::fs::write(path.join("buzz/hello.txt"), "buzz!")
                .await
                .unwrap();
            tokio::fs::write(path.join("buzz/hello.secret"), "buzz!")
                .await
                .unwrap();
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {
                        name: "foo",
                    };

                    export const globbed = Brioche.glob("fizz", "**/*.txt");
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    let foo_path = brioche
        .data_dir
        .join("projects")
        .join(foo_dep_hash.to_string())
        .canonicalize()
        .unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    assert!(foo_path.join("fizz/hello.md").exists());
    assert!(foo_path.join("buzz/hello.txt").exists());
    assert!(!foo_path.join("buzz/hello.secret").exists());

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_registry_dep_with_subdir_brioche_glob() -> anyhow::Result<()>
{
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::create_dir_all(path.join("subdir/fizz"))
                .await
                .unwrap();
            tokio::fs::write(path.join("subdir/fizz/hello.md"), "fizz!")
                .await
                .unwrap();
            tokio::fs::create_dir_all(path.join("subdir/buzz"))
                .await
                .unwrap();
            tokio::fs::write(path.join("subdir/buzz/hello.txt"), "buzz!")
                .await
                .unwrap();
            tokio::fs::write(path.join("subdir/buzz/hello.secret"), "buzz!")
                .await
                .unwrap();
            tokio::fs::write(
                path.join("subdir/files.bri"),
                r#"
                    export const globbed = Brioche.glob("fizz", "**/*.txt");
                "#,
            )
            .await
            .unwrap();
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {
                        name: "foo",
                    };

                    export { globbed } from "./subdir/files.bri";
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    let foo_path = brioche
        .data_dir
        .join("projects")
        .join(foo_dep_hash.to_string())
        .canonicalize()
        .unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    assert!(foo_path.join("subdir/files.bri").exists());
    assert!(foo_path.join("subdir/fizz/hello.md").exists());
    assert!(foo_path.join("subdir/buzz/hello.txt").exists());
    assert!(!foo_path.join("subdir/buzz/hello.secret").exists());

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_registry_dep_with_brioche_download() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

    let mut server = mockito::Server::new_async().await;
    let server_url = server.url();

    let hello = "hello";
    let hello_endpoint = server
        .mock("GET", "/file.txt")
        .with_body(hello)
        .expect(1)
        .create();

    let download_url = format!("{server_url}/file.txt");

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {
                        name: "foo",
                    };

                    export const hello = Brioche.download("<DOWNLOAD_URL>");
                "#
                .replace("<DOWNLOAD_URL>", &download_url),
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let foo_dep_hash = projects.project_dependencies(project_hash).unwrap()["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    let foo_path = brioche
        .data_dir
        .join("projects")
        .join(foo_dep_hash.to_string())
        .canonicalize()
        .unwrap();
    assert!(
        projects
            .local_paths(foo_dep_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(foo_dep.dependencies.is_empty());

    mock_foo_latest.assert_async().await;

    hello_endpoint.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_registry_deps_with_common_children() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const a = 1;
                    export const b = 2;
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let fizz_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export { a } from "foo";
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_fizz_latest = context
        .mock_registry_publish_tag("fizz", "latest", fizz_hash)
        .create_async()
        .await;

    let buzz_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export { b } from "foo";
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_buzz_latest = context
        .mock_registry_publish_tag("buzz", "latest", buzz_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import { a } from "fizz";
                import { b } from "buzz";
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let fizz_dep_hash = projects.project_dependencies(project_hash).unwrap()["fizz"];
    let fizz_path = brioche
        .data_dir
        .join("projects")
        .join(fizz_dep_hash.to_string())
        .canonicalize()
        .unwrap();
    assert!(
        projects
            .local_paths(fizz_dep_hash)
            .unwrap()
            .contains(&fizz_path)
    );

    let buzz_dep_hash = projects.project_dependencies(project_hash).unwrap()["buzz"];
    let buzz_path = brioche
        .data_dir
        .join("projects")
        .join(buzz_dep_hash.to_string())
        .canonicalize()
        .unwrap();
    assert!(
        projects
            .local_paths(buzz_dep_hash)
            .unwrap()
            .contains(&buzz_path)
    );

    let fizz_foo_dep_hash = projects.project_dependencies(fizz_dep_hash).unwrap()["foo"];
    let buzz_foo_dep_hash = projects.project_dependencies(buzz_dep_hash).unwrap()["foo"];
    assert_eq!(fizz_foo_dep_hash, buzz_foo_dep_hash);
    assert_eq!(fizz_foo_dep_hash, foo_hash);

    let foo_project = projects.project(foo_hash).unwrap();
    let foo_project_path = brioche
        .data_dir
        .join("projects")
        .join(foo_hash.to_string())
        .canonicalize()
        .unwrap();
    assert!(
        projects
            .local_paths(foo_hash)
            .unwrap()
            .contains(&foo_project_path)
    );

    assert!(foo_project.dependencies.is_empty());

    mock_foo_latest.expect_at_least(1).assert_async().await;
    mock_fizz_latest.assert_async().await;
    mock_buzz_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_workspace_registry_dep() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), true).await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./foo".parse()?, "./bar".parse()?],
            },
        )
        .await;

    let foo_dir = context.mkdir("myworkspace/foo").await;
    context
        .write_file("myworkspace/foo/project.bri", "export const project = {};")
        .await;

    let bar_dir = context.mkdir("myworkspace/bar").await;
    context
        .write_file(
            "myworkspace/bar/project.bri",
            r#"
            export const project = {
                dependencies: {
                    foo: "*",
                },
            };
        "#,
        )
        .await;

    let projects = brioche_core::project::Projects::default();
    let bar_hash = projects
        .load(
            &brioche,
            &bar_dir,
            ProjectValidation::Standard,
            ProjectLocking::Unlocked,
        )
        .await
        .expect("failed to load bar project");
    let foo_hash = projects
        .load(
            &brioche,
            &foo_dir,
            ProjectValidation::Standard,
            ProjectLocking::Unlocked,
        )
        .await
        .expect("failed to load foo project");

    let bar_project = projects.project(bar_hash).expect("bar project not found");

    assert_eq!(
        bar_project.dependencies.get("foo"),
        Some(&DependencyRef::Project(foo_hash))
    );

    let bar_project_artifact = brioche_core::project::artifact::create_artifact_with_projects(
        &brioche,
        &projects,
        &[bar_hash],
    )
    .await
    .expect("failed to create artifact for bar");
    let bar_project_artifact = brioche_core::recipe::Artifact::Directory(bar_project_artifact);
    let bar_project_artifact_hash = bar_project_artifact.hash();
    brioche_core::cache::save_artifact(&brioche, bar_project_artifact)
        .await
        .expect("failed to save bar project artifact to cache");
    brioche_core::cache::save_project_artifact_hash(&brioche, bar_hash, bar_project_artifact_hash)
        .await
        .expect("failed to save bar project artifact hash to cache");

    let mock_bar_latest = context
        .mock_registry_publish_tag("bar", "latest", bar_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        bar: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let bar_dep_hash = projects.project_dependencies(project_hash).unwrap()["bar"];
    let bar_dep_project = projects.project(bar_dep_hash).unwrap();
    assert_eq!(bar_project, bar_dep_project);

    mock_bar_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_locked_registry_dep() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

    let bar_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {};
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    let mock_bar_latest = context
        .mock_registry_publish_tag("bar", "latest", bar_hash)
        .create_async()
        .await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {
                        dependencies: {
                            bar: "*",
                        },
                    };
                "#,
            )
            .await
            .unwrap();
        })
        .await;

    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    let project = projects.project(project_hash).unwrap();
    projects.commit_dirty_lockfiles().await?;
    assert!(
        projects
            .local_paths(project_hash)
            .unwrap()
            .contains(&project_dir)
    );

    let project_lockfile_path = project_dir.join("brioche.lock");
    assert!(tokio::fs::try_exists(&project_lockfile_path).await?);

    let project_lockfile_contents = tokio::fs::read_to_string(&project_lockfile_path).await?;
    let project_lockfile: brioche_core::project::Lockfile =
        serde_json::from_str(&project_lockfile_contents)?;

    let foo_lockfile_dep_hash = &project.dependencies["foo"];
    assert_eq!(foo_lockfile_dep_hash, &DependencyRef::Project(foo_hash));

    // "foo" should be in the lockfile
    assert!(project.dependencies.contains_key("foo"));
    assert!(project_lockfile.dependencies.contains_key("foo"));

    mock_foo_latest.assert_async().await;

    // `bar` may have been fetched multiple times while publishing `foo`
    mock_bar_latest.expect_at_least(1).assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_complex() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let main_project_dir = context.mkdir("mainproject").await;
    context
        .write_file(
            "mainproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        depproject: {
                            path: "../depproject",
                        },
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let dep_project_dir = context.mkdir("depproject").await;
    context
        .write_file(
            "depproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let (bar_hash, bar_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {};
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("bar", "latest", bar_hash)
        .create_async()
        .await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                export const project = {
                    dependencies: {
                        bar: "*",
                    },
                };
            "#,
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &main_project_dir).await?;

    let main_dep_project_hash = projects.project_dependencies(project_hash).unwrap()["depproject"];

    let main_foo_project_hash = projects.project_dependencies(project_hash).unwrap()["foo"];

    let main_dep_foo_project_hash = projects
        .project_dependencies(main_dep_project_hash)
        .unwrap()["foo"];

    let main_foo_bar_project_hash = projects
        .project_dependencies(main_foo_project_hash)
        .unwrap()["bar"];

    let main_dep_foo_bar_project_hash = projects
        .project_dependencies(main_dep_foo_project_hash)
        .unwrap()["bar"];

    assert!(
        projects
            .local_paths(main_dep_project_hash)
            .unwrap()
            .contains(&dep_project_dir)
    );
    assert!(
        projects
            .local_paths(main_foo_project_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(
        projects
            .local_paths(main_dep_foo_project_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(
        projects
            .local_paths(main_foo_bar_project_hash)
            .unwrap()
            .contains(&bar_path)
    );
    assert!(
        projects
            .local_paths(main_dep_foo_bar_project_hash)
            .unwrap()
            .contains(&bar_path)
    );

    assert_eq!(main_foo_project_hash, main_dep_foo_project_hash);
    assert_eq!(main_foo_bar_project_hash, main_dep_foo_bar_project_hash);

    Ok(())
}

#[tokio::test]
async fn test_project_load_complex_implied() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let main_project_dir = context.mkdir("mainproject").await;
    context
        .write_file(
            "mainproject/project.bri",
            r#"
                import "depproject";
                import "foo";

                export const project = {
                    dependencies: {
                        depproject: {
                            path: "../depproject",
                        },
                    },
                };
            "#,
        )
        .await;

    let dep_project_dir = context.mkdir("depproject").await;
    context
        .write_file(
            "depproject/project.bri",
            r#"
                import "foo";
            "#,
        )
        .await;

    let (bar_hash, bar_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    // Empty project
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("bar", "latest", bar_hash)
        .create_async()
        .await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    import "bar";
                "#,
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &main_project_dir).await?;

    let main_dep_project_hash = projects.project_dependencies(project_hash).unwrap()["depproject"];

    let main_foo_project_hash = projects.project_dependencies(project_hash).unwrap()["foo"];

    let main_dep_foo_project_hash = projects
        .project_dependencies(main_dep_project_hash)
        .unwrap()["foo"];

    let main_foo_bar_project_hash = projects
        .project_dependencies(main_foo_project_hash)
        .unwrap()["bar"];

    let main_dep_foo_bar_project_hash = projects
        .project_dependencies(main_dep_foo_project_hash)
        .unwrap()["bar"];

    assert!(
        projects
            .local_paths(main_dep_project_hash)
            .unwrap()
            .contains(&dep_project_dir)
    );
    assert!(
        projects
            .local_paths(main_foo_project_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(
        projects
            .local_paths(main_dep_foo_project_hash)
            .unwrap()
            .contains(&foo_path)
    );
    assert!(
        projects
            .local_paths(main_foo_bar_project_hash)
            .unwrap()
            .contains(&bar_path)
    );
    assert!(
        projects
            .local_paths(main_dep_foo_bar_project_hash)
            .unwrap()
            .contains(&bar_path)
    );

    assert_eq!(main_foo_project_hash, main_dep_foo_project_hash);
    assert_eq!(main_foo_bar_project_hash, main_dep_foo_bar_project_hash);

    Ok(())
}

#[tokio::test]
async fn test_project_load_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    // project.bri does not exist
    let project_dir = context.mkdir("myproject").await;

    let result = brioche_test_support::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_path_dep_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        mydep: {
                            path: "../mydep",
                        },
                    },
                };
            "#,
        )
        .await;

    // project.bri does not exist
    let _dep_dir = context.mkdir("mydep").await;

    let result = brioche_test_support::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_dep_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let result = brioche_test_support::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_dep_implied_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import "foo";
            "#,
        )
        .await;

    let result = brioche_test_support::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_brioche_include_outside_of_project() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context.write_file("foo", "secret!!!").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    includeFile: (path) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "include",
                                        include: "file",
                                        path,
                                    },
                                );
                            },
                        };
                    }
                }

                export default () => {
                    return Brioche.includeFile("../foo");
                };
            "#,
        )
        .await;

    let result = brioche_test_support::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_brioche_include_directory_as_file_error() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context.write_file("myproject/foo/not_a_file", "...").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    includeFile: (path) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "include",
                                        include: "file",
                                        path,
                                    },
                                );
                            },
                        };
                    }
                }

                export default () => {
                    return Brioche.includeFile("foo");
                };
            "#,
        )
        .await;

    let result = brioche_test_support::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_brioche_include_file_as_directory_error() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context.write_file("myproject/foo", "not a directory").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    includeDirectory: (path) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "include",
                                        include: "directory",
                                        path,
                                    },
                                );
                            },
                        };
                    }
                }

                export default () => {
                    return Brioche.includeDirectory("foo");
                };
            "#,
        )
        .await;

    let result = brioche_test_support::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_registry_dep_hash_mismatch_error() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();

    let foo_hash = {
        let (brioche, context) = brioche_test_with_cache(cache.clone(), true).await;

        // Create a project
        let foo_project_dir = context.mkdir("foo").await;
        context
            .write_file(
                "foo/project.bri",
                r#"
                    // Foo
                "#,
            )
            .await;
        let (projects, foo_hash) =
            brioche_test_support::load_project(&brioche, &foo_project_dir).await?;

        // Create an artifact from the project
        let mut foo_project_artifact =
            brioche_core::project::artifact::create_artifact_with_projects(
                &brioche,
                &projects,
                &[foo_hash],
            )
            .await?;

        // Read the current `project.bri` file from the current blob to
        // validate it exists at the path we expect
        let artifact_project_bri_path = format!("{foo_hash}/project.bri");
        let previous_project_bri_blob = foo_project_artifact
            .get(&brioche, artifact_project_bri_path.as_bytes())
            .await
            .unwrap();
        assert_matches!(previous_project_bri_blob, Some(_));

        // Change the artifact so thta the project hash no longer matches
        // the expected hash
        let new_project_bri_blob = brioche_test_support::blob(
            &brioche,
            r#"
                // Foo
                // (This file has been modified so the hash shouldn't match anymore)
                export const uhOh = "uh oh";
            "#,
        )
        .await;
        foo_project_artifact
            .insert(
                &brioche,
                artifact_project_bri_path.as_bytes(),
                Some(brioche_test_support::file(new_project_bri_blob, false)),
            )
            .await?;

        // Publish the artifact to the cache with the (incorrect) project hash
        let foo_artifact = brioche_core::recipe::Artifact::Directory(foo_project_artifact);
        let foo_artifact_hash = foo_artifact.hash();
        brioche_core::cache::save_artifact(&brioche, foo_artifact).await?;
        brioche_core::cache::save_project_artifact_hash(&brioche, foo_hash, foo_artifact_hash)
            .await?;

        foo_hash
    };

    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    // Try loading the project. This should fail because `foo` doesn't
    // have the right hash
    let result = brioche_test_support::load_project(&brioche, &project_dir)
        .await
        .map(|(_, hash)| hash);
    assert_matches!(result, Err(_));

    mock_foo_latest.assert_async().await;

    Ok(())
}

async fn brioche_test_with_cache(
    cache: Arc<dyn object_store::ObjectStore>,
    writable: bool,
) -> (Brioche, TestContext) {
    brioche_test_support::brioche_test_with(|builder| {
        builder.cache_client(brioche_core::cache::CacheClient {
            store: Some(cache),
            writable,
            ..Default::default()
        })
    })
    .await
}
