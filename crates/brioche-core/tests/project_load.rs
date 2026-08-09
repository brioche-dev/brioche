#![allow(clippy::similar_names)]

use std::collections::HashSet;

use assert_matches::assert_matches;
use brioche_core::project::{ProjectIssue, ProjectSpecifier, load::LoadModuleError};
use pretty_assertions::assert_eq;

#[tokio::test]
async fn test_project_load_simple() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r"
                export const project = {};
            ",
        )
        .await;

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let dependencies = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert!(dependencies.is_empty());
}

#[tokio::test]
async fn test_project_load_simple_no_definition() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context.write_file("myproject/project.bri", r"").await;

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let dependencies = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert!(dependencies.is_empty());
}

#[tokio::test]
async fn test_project_load_workspace_dep() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./foo".parse().unwrap(), "./bar".parse().unwrap()],
            },
        )
        .await;

    let workspace_foo_dir = context.mkdir("myworkspace/foo").await;
    context
        .write_file(
            "myworkspace/foo/project.bri",
            r"
                // Workspace foo
            ",
        )
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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let dependencies = brioche_core::project::get_dependencies(brioche, project_ref);

    let foo_specifier = brioche_core::project::get_specifier(brioche, dependencies["foo"]);
    assert_eq!(
        foo_specifier,
        brioche_test_support::project_specifier_for_path(&workspace_foo_dir)
    );

    let foo_dependencies = brioche_core::project::get_dependencies(brioche, dependencies["foo"]);
    assert!(foo_dependencies.is_empty());
}

#[tokio::test]
async fn test_project_load_path_dep() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: {
                            path: "../foo",
                        },
                        bar: {
                            path: "../bar",
                        },
                    },
                };
            "#,
        )
        .await;

    let foo_dir = context.mkdir("foo").await;
    context
        .write_file(
            "foo/project.bri",
            r#"
                import "baz";
                export const project = {
                    name: "foo",
                    dependencies: {
                        baz: {
                            path: "../baz",
                        },
                    },
                };
            "#,
        )
        .await;

    let bar_dir = context.mkdir("bar").await;
    context
        .write_file(
            "bar/project.bri",
            r#"
                export const project = {
                    name: "bar",
                    dependencies: {
                        baz: {
                            path: "../baz",
                        },
                    },
                };
            "#,
        )
        .await;

    let baz_dir = context.mkdir("baz").await;
    context
        .write_file(
            "baz/project.bri",
            r#"
                export const project = {
                    name: "baz"
                };
            "#,
        )
        .await;

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        2,
        "expected to get 2 project dependencies, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    assert_eq!(
        foo_specifier,
        brioche_test_support::project_specifier_for_path(&foo_dir)
    );
    assert_eq!(
        foo_deps.len(),
        1,
        "expected to get 1 dependency for foo, got: {foo_deps:#?}"
    );

    let bar_ref = project_deps["bar"];
    let bar_specifier = brioche_core::project::get_specifier(&*brioche.read().await, bar_ref);
    let bar_deps = brioche_core::project::get_dependencies(&*brioche.read().await, bar_ref);
    assert_eq!(
        bar_specifier,
        brioche_test_support::project_specifier_for_path(&bar_dir)
    );
    assert_eq!(
        bar_deps.len(),
        1,
        "expected to get 1 dependency for bar, got: {bar_deps:#?}"
    );

    assert_eq!(foo_deps["baz"], bar_deps["baz"]);
    let baz_ref = foo_deps["baz"];
    let baz_specifier = brioche_core::project::get_specifier(&*brioche.read().await, baz_ref);
    let baz_deps = brioche_core::project::get_dependencies(&*brioche.read().await, baz_ref);
    assert_eq!(
        baz_specifier,
        brioche_test_support::project_specifier_for_path(&baz_dir)
    );
    assert_eq!(
        baz_deps.len(),
        0,
        "expected to get 0 dependencies for baz, got: {baz_deps:#?}"
    );
}

#[tokio::test]
async fn test_project_load_local_registry_dep() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r"
                    export const project = {};
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;
}

#[tokio::test]
async fn test_project_load_local_registry_dep_implied() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r"
                    export const project = {};
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;
}

#[tokio::test]
async fn test_project_load_local_registry_dep_implied_nested() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r"
                    // foo
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;
}

#[tokio::test]
async fn test_project_load_local_registry_dep_imported() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r"
                    export const project = {};
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;
}

#[tokio::test]
async fn test_project_load_remote_registry_dep() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r"
                    export const project = {};
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_path = brioche
        .data_dir()
        .join("projects")
        .join(foo_hash.to_string())
        .canonicalize()
        .unwrap();

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;
}

#[tokio::test]
async fn test_project_load_remote_registry_dep_with_brioche_include() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_path = brioche
        .data_dir()
        .join("projects")
        .join(foo_hash.to_string())
        .canonicalize()
        .unwrap();

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;

    let fizz_path = foo_path.join("fizz");
    let buzz_hello_world_path = foo_path.join("buzz/hello/world.txt");

    assert!(tokio::fs::try_exists(&fizz_path).await.unwrap());
    assert!(tokio::fs::try_exists(&buzz_hello_world_path).await.unwrap());
}

#[tokio::test]
async fn test_project_load_remote_registry_dep_with_brioche_glob() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_path = brioche
        .data_dir()
        .join("projects")
        .join(foo_hash.to_string())
        .canonicalize()
        .unwrap();

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;

    assert!(
        tokio::fs::try_exists(foo_path.join("fizz/hello.md"))
            .await
            .unwrap()
    );
    assert!(
        tokio::fs::try_exists(foo_path.join("buzz/hello.txt"))
            .await
            .unwrap()
    );
    assert!(
        !tokio::fs::try_exists(foo_path.join("buzz/hello.secret"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn test_project_load_remote_registry_dep_with_subdir_brioche_glob() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_path = brioche
        .data_dir()
        .join("projects")
        .join(foo_hash.to_string())
        .canonicalize()
        .unwrap();

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;

    assert!(
        tokio::fs::try_exists(foo_path.join("subdir/files.bri"))
            .await
            .unwrap()
    );
    assert!(
        tokio::fs::try_exists(foo_path.join("subdir/fizz/hello.md"))
            .await
            .unwrap()
    );
    assert!(
        tokio::fs::try_exists(foo_path.join("subdir/buzz/hello.txt"))
            .await
            .unwrap()
    );
    assert!(
        !tokio::fs::try_exists(foo_path.join("subdir/buzz/hello.secret"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn test_project_load_remote_registry_dep_with_brioche_download() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_path = brioche
        .data_dir()
        .join("projects")
        .join(foo_hash.to_string())
        .canonicalize()
        .unwrap();

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;

    hello_endpoint.assert_async().await;
}

#[tokio::test]
async fn test_project_load_shared_download_url_across_path_deps_fetched_once() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let mut server = mockito::Server::new_async().await;
    let server_url = server.url();

    let hello = "hello";
    let hello_endpoint = server
        .mock("GET", "/file.txt")
        .with_body(hello)
        .expect(1)
        .create();

    let download_url = format!("{server_url}/file.txt");

    context.mkdir("foodep").await;
    context
        .write_file(
            "foodep/project.bri",
            r#"
                export const project = {};
                export const hello = Brioche.download("<DOWNLOAD_URL>");
            "#
            .replace("<DOWNLOAD_URL>", &download_url),
        )
        .await;

    context.mkdir("bardep").await;
    context
        .write_file(
            "bardep/project.bri",
            r#"
                export const project = {};
                export const hello = Brioche.download("<DOWNLOAD_URL>");
            "#
            .replace("<DOWNLOAD_URL>", &download_url),
        )
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foodep: {
                            path: "../foodep",
                        },
                        bardep: {
                            path: "../bardep",
                        },
                    },
                };
            "#,
        )
        .await;

    let _project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    hello_endpoint.assert_async().await;
}

#[tokio::test]
async fn test_project_load_remote_registry_deps_with_common_children() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r"
                    export const a = 1;
                    export const b = 2;
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        2,
        "expected to get 2 project dependencies, got: {project_deps:#?}"
    );

    let fizz_path = brioche
        .data_dir()
        .join("projects")
        .join(fizz_hash.to_string())
        .canonicalize()
        .unwrap();
    let fizz_ref = project_deps["fizz"];
    let fizz_specifier = brioche_core::project::get_specifier(&*brioche.read().await, fizz_ref);
    let fizz_deps = brioche_core::project::get_dependencies(&*brioche.read().await, fizz_ref);
    let fizz_local_path =
        brioche_core::project::local_project_path(&*brioche.read().await, fizz_ref)
            .to_system_path()
            .unwrap();
    assert_eq!(fizz_specifier, ProjectSpecifier::Hash(fizz_hash));
    assert_eq!(fizz_local_path, fizz_path);
    assert_eq!(
        fizz_deps.len(),
        1,
        "expected to get 1 dependency for fizz, got: {fizz_deps:#?}"
    );

    let buzz_path = brioche
        .data_dir()
        .join("projects")
        .join(buzz_hash.to_string())
        .canonicalize()
        .unwrap();
    let buzz_ref = project_deps["buzz"];
    let buzz_specifier = brioche_core::project::get_specifier(&*brioche.read().await, buzz_ref);
    let buzz_deps = brioche_core::project::get_dependencies(&*brioche.read().await, buzz_ref);
    let buzz_local_path =
        brioche_core::project::local_project_path(&*brioche.read().await, buzz_ref)
            .to_system_path()
            .unwrap();
    assert_eq!(buzz_specifier, ProjectSpecifier::Hash(buzz_hash));
    assert_eq!(buzz_local_path, buzz_path);
    assert_eq!(
        buzz_deps.len(),
        1,
        "expected to get 1 dependency for buzz, got: {buzz_deps:#?}"
    );

    let fizz_foo_dep_ref =
        brioche_core::project::get_dependencies(&*brioche.read().await, fizz_ref)["foo"];
    let buzz_foo_dep_ref =
        brioche_core::project::get_dependencies(&*brioche.read().await, buzz_ref)["foo"];
    let foo_ref = brioche_core::project::get_project_by_specifier(
        &*brioche.read().await,
        &ProjectSpecifier::Hash(foo_hash),
    )
    .unwrap();
    assert_eq!(fizz_foo_dep_ref, buzz_foo_dep_ref);
    assert_eq!(fizz_foo_dep_ref, foo_ref);

    let foo_path = brioche
        .data_dir()
        .join("projects")
        .join(foo_hash.to_string())
        .canonicalize()
        .unwrap();

    let foo_specifier = brioche_core::project::get_specifier(&*brioche.read().await, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(&*brioche.read().await, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(&*brioche.read().await, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.expect_at_least(1).assert_async().await;
    mock_fizz_latest.assert_async().await;
    mock_buzz_latest.assert_async().await;
}

#[tokio::test]
async fn test_project_load_remote_workspace_registry_dep() {
    let cache = brioche_test_support::new_cache();
    let bar_hash;

    {
        let (brioche, context) =
            brioche_test_support::brioche_test_with_cache(cache.clone(), true).await;

        context
            .write_toml(
                "myworkspace/brioche_workspace.toml",
                &brioche_core::project::WorkspaceDefinition {
                    members: vec!["./foo".parse().unwrap(), "./bar".parse().unwrap()],
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

        let bar_ref = brioche_test_support::load_project(&brioche, &bar_dir).await;
        let foo_ref = brioche_test_support::load_project(&brioche, &foo_dir).await;

        let bar_deps = brioche_core::project::get_dependencies(&*brioche.read().await, bar_ref);
        assert_eq!(
            bar_deps.len(),
            1,
            "expected bar to have 1 dependency, got: {bar_deps:#?}"
        );
        assert_eq!(bar_deps["foo"], foo_ref);

        bar_hash = brioche_core::project::hash::hash_project(&mut *brioche.write().await, bar_ref)
            .await
            .expect("failed to hash bar project");
        let bar_project_artifact = brioche_core::project::artifact::create_project_artifact(
            &mut *brioche.write().await,
            bar_ref,
        )
        .await
        .expect("failed to create artifact for bar");
        let bar_project_artifact_hash = brioche_core::recipe::hash::hash_recipe(
            &mut *brioche.write().await,
            bar_project_artifact,
        );
        brioche_core::cache::save_artifact(&mut *brioche.write().await, bar_project_artifact)
            .await
            .expect("failed to save bar project artifact to cache");
        brioche_core::cache::save_project_artifact_hash(
            &mut *brioche.write().await,
            bar_hash,
            bar_project_artifact_hash,
        )
        .await
        .expect("failed to save bar project artifact hash to cache");
    }

    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected project to have 1 dependency, got: {project_deps:#?}"
    );

    let bar_dep_ref = project_deps["bar"];
    let bar_dep_hash =
        brioche_core::project::hash::hash_project(&mut *brioche.write().await, bar_dep_ref)
            .await
            .expect("failed to hash bar");
    assert_eq!(bar_dep_hash, bar_hash);

    mock_bar_latest.assert_async().await;
}

#[tokio::test]
async fn test_project_load_locked_registry_dep() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

    let bar_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r"
                    export const project = {};
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let committed_projects =
        brioche_core::project::lock::commit_all_dirty_lockfiles(&mut *brioche.write().await)
            .await
            .expect("failed to commit dirty lockfiles");
    assert_eq!(committed_projects, HashSet::from_iter([project_ref]));

    let project_lockfile_path = project_dir.join("brioche.lock");
    assert!(
        tokio::fs::try_exists(&project_lockfile_path)
            .await
            .expect("lockfile not found")
    );

    let project_lockfile_contents = tokio::fs::read_to_string(&project_lockfile_path)
        .await
        .expect("failed to read lockfile");
    let project_lockfile: brioche_core::project::Lockfile =
        serde_json::from_str(&project_lockfile_contents).expect("failed to parse lockfile");

    let project_deps = brioche_core::project::get_dependencies(&*brioche.read().await, project_ref);
    let foo_dep_ref = project_deps["foo"];
    let foo_dep_hash =
        brioche_core::project::hash::hash_project(&mut *brioche.write().await, foo_dep_ref)
            .await
            .unwrap();

    assert_eq!(foo_dep_hash, foo_hash);

    // "foo" should be in the lockfile
    assert!(project_lockfile.dependencies.contains_key("foo"));
    assert_eq!(project_lockfile.dependencies["foo"], foo_hash);

    mock_foo_latest.assert_async().await;

    // `bar` may have been fetched multiple times while publishing `foo`
    mock_bar_latest.expect_at_least(1).assert_async().await;
}

#[tokio::test]
async fn test_project_load_complex() {
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
                r"
                    export const project = {};
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &main_project_dir).await;

    let main_dep_project_ref =
        brioche_core::project::get_dependencies(&*brioche.read().await, project_ref)["depproject"];
    let main_foo_project_ref =
        brioche_core::project::get_dependencies(&*brioche.read().await, project_ref)["foo"];
    let main_dep_foo_project_ref = brioche_core::project::get_dependencies(
        &*brioche.read().await,
        main_dep_project_ref,
    )["foo"];
    let main_foo_bar_project_ref = brioche_core::project::get_dependencies(
        &*brioche.read().await,
        main_foo_project_ref,
    )["bar"];
    let main_dep_foo_bar_project_ref = brioche_core::project::get_dependencies(
        &*brioche.read().await,
        main_dep_foo_project_ref,
    )["bar"];

    assert_eq!(
        brioche_core::project::local_project_path(&*brioche.read().await, main_dep_project_ref)
            .to_system_path()
            .unwrap(),
        dep_project_dir
    );
    assert_eq!(
        brioche_core::project::local_project_path(&*brioche.read().await, main_foo_project_ref)
            .to_system_path()
            .unwrap(),
        foo_path
    );
    assert_eq!(
        brioche_core::project::local_project_path(&*brioche.read().await, main_dep_foo_project_ref)
            .to_system_path()
            .unwrap(),
        foo_path
    );
    assert_eq!(
        brioche_core::project::local_project_path(&*brioche.read().await, main_foo_bar_project_ref)
            .to_system_path()
            .unwrap(),
        bar_path
    );
    assert_eq!(
        brioche_core::project::local_project_path(
            &*brioche.read().await,
            main_dep_foo_bar_project_ref
        )
        .to_system_path()
        .unwrap(),
        bar_path
    );

    assert_eq!(main_foo_project_ref, main_dep_foo_project_ref);
    assert_eq!(main_foo_bar_project_ref, main_dep_foo_bar_project_ref);
}

#[tokio::test]
async fn test_project_load_complex_implied() {
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
                r"
                    // Empty project
                ",
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

    let project_ref = brioche_test_support::load_project(&brioche, &main_project_dir).await;

    let main_dep_project_ref =
        brioche_core::project::get_dependencies(&*brioche.read().await, project_ref)["depproject"];
    let main_foo_project_ref =
        brioche_core::project::get_dependencies(&*brioche.read().await, project_ref)["foo"];
    let main_dep_foo_project_ref = brioche_core::project::get_dependencies(
        &*brioche.read().await,
        main_dep_project_ref,
    )["foo"];
    let main_foo_bar_project_ref = brioche_core::project::get_dependencies(
        &*brioche.read().await,
        main_foo_project_ref,
    )["bar"];
    let main_dep_foo_bar_project_ref = brioche_core::project::get_dependencies(
        &*brioche.read().await,
        main_dep_foo_project_ref,
    )["bar"];

    assert_eq!(
        brioche_core::project::local_project_path(&*brioche.read().await, main_dep_project_ref)
            .to_system_path()
            .unwrap(),
        dep_project_dir
    );
    assert_eq!(
        brioche_core::project::local_project_path(&*brioche.read().await, main_foo_project_ref)
            .to_system_path()
            .unwrap(),
        foo_path
    );
    assert_eq!(
        brioche_core::project::local_project_path(&*brioche.read().await, main_dep_foo_project_ref)
            .to_system_path()
            .unwrap(),
        foo_path
    );
    assert_eq!(
        brioche_core::project::local_project_path(&*brioche.read().await, main_foo_bar_project_ref)
            .to_system_path()
            .unwrap(),
        bar_path
    );
    assert_eq!(
        brioche_core::project::local_project_path(
            &*brioche.read().await,
            main_dep_foo_bar_project_ref
        )
        .to_system_path()
        .unwrap(),
        bar_path
    );

    assert_eq!(main_foo_project_ref, main_dep_foo_project_ref);
    assert_eq!(main_foo_bar_project_ref, main_dep_foo_bar_project_ref);
}

#[tokio::test]
async fn test_project_load_not_found() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    // project.bri does not exist
    let project_dir = context.mkdir("myproject").await;

    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::LoadModuleError {
            error: LoadModuleError::IoError { .. },
            ..
        }]
    );
}

#[tokio::test]
async fn test_project_load_path_dep_not_found() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        not_found_1: {
                            path: "../not_found_1",
                        },
                        foo: {
                            path: "../foo",
                        },
                    },
                };
            "#,
        )
        .await;

    let foo_dir = context.mkdir("foo").await;
    context
        .write_file(
            "foo/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        not_found_2: {
                            path: "../not_found_2",
                        },
                    },
                };
            "#,
        )
        .await;

    let not_found_1_dir = context.mkdir("not_found_1").await;
    let not_found_2_dir = context.path("not_found_2");

    // The directory `not_found_1` exists, but does not contain a root
    // module. The directory `not_found_2` does not exist

    let project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;
    let foo_ref = brioche_core::project::get_project_by_specifier(
        &*brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&foo_dir),
    )
    .unwrap();

    let project_root_module =
        brioche_core::project::get_root_module(&*brioche.read().await, project_ref)
            .expect("project root module not found");
    let foo_root_module = brioche_core::project::get_root_module(&*brioche.read().await, foo_ref)
        .expect("foo root module not found");

    let brioche = &*brioche.read().await;
    let mut issues = brioche_test_support::get_all_issues(brioche);
    assert_eq!(issues.len(), 2, "expected 2 issues, got: {issues:#?}");

    let project_issue = brioche_test_support::take_where(&mut issues, |issue| {
        issue
            .location()
            .is_some_and(|location| location.source == project_root_module.into())
    });
    let foo_issue = brioche_test_support::take_where(&mut issues, |issue| {
        issue
            .location()
            .is_some_and(|location| location.source == foo_root_module.into())
    });
    assert!(issues.is_empty());

    let ProjectIssue::LoadModuleError {
        error:
            LoadModuleError::IoError {
                path: project_issue_path,
                ..
            },
        ..
    } = project_issue
    else {
        panic!("expected LoadModuleError::IoError, got: {project_issue:#?}");
    };
    let ProjectIssue::LoadModuleError {
        error: LoadModuleError::IoError {
            path: foo_issue_path,
            ..
        },
        ..
    } = foo_issue
    else {
        panic!("expected LoadModuleError::IoError, got: {foo_issue:#?}");
    };

    assert_eq!(*project_issue_path, not_found_1_dir.join("project.bri"));
    assert_eq!(*foo_issue_path, not_found_2_dir.join("project.bri"));
}

#[tokio::test]
async fn test_project_load_dep_not_found() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

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

    let mock_foo_latest_not_found = context
        .mock_registry_tag_response("foo", "latest")
        .with_status(404)
        .with_body("not found")
        .create_async()
        .await;

    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    mock_foo_latest_not_found.assert_async().await;
}

#[tokio::test]
async fn test_project_load_dep_implied_not_found() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import "foo";
            "#,
        )
        .await;

    let mock_foo_latest_not_found = context
        .mock_registry_tag_response("foo", "latest")
        .with_status(404)
        .with_body("not found")
        .create_async()
        .await;

    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(&issues[..], [ProjectIssue::DependencyNotFound { dependency, .. }] if dependency == "foo");

    mock_foo_latest_not_found.assert_async().await;
}

#[tokio::test]
async fn test_project_load_dep_registry_error() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

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

    let mock_foo_latest_error = context
        .mock_registry_tag_response("foo", "latest")
        .with_status(400)
        .with_body("bad request")
        .create_async()
        .await;

    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(&issues[..], [ProjectIssue::RegistryError { .. }]);

    mock_foo_latest_error.assert_async().await;
}

#[tokio::test]
async fn test_project_load_dep_implied_registry_error() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import "foo";
            "#,
        )
        .await;

    let mock_foo_latest_error = context
        .mock_registry_tag_response("foo", "latest")
        .with_status(400)
        .with_body("bad request")
        .create_async()
        .await;

    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(&issues[..], [ProjectIssue::RegistryError { .. }]);

    mock_foo_latest_error.assert_async().await;
}

#[tokio::test]
async fn test_project_load_brioche_include_outside_of_project_error() {
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

    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::StaticIncludeEscapesProjectPath { .. }]
    );
}

#[tokio::test]
async fn test_project_load_brioche_include_directory_as_file_error() {
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

    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::StaticIncludeExpectedFile { .. }]
    );
}

#[tokio::test]
async fn test_project_load_brioche_include_file_as_directory_error() {
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

    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::StaticIncludeExpectedDirectory { .. }]
    );
}

#[tokio::test]
async fn test_project_load_with_remote_registry_dep_hash_mismatch_error() {
    let cache = brioche_test_support::new_cache();

    let foo_hash = {
        let (brioche, context) =
            brioche_test_support::brioche_test_with_cache(cache.clone(), true).await;

        // Create a project
        let foo_project_dir = context.mkdir("foo").await;
        context
            .write_file(
                "foo/project.bri",
                r"
                    // Foo
                ",
            )
            .await;
        let foo_ref = brioche_test_support::load_project(&brioche, &foo_project_dir).await;
        let foo_hash =
            brioche_core::project::hash::hash_project(&mut *brioche.write().await, foo_ref)
                .await
                .unwrap();

        // Create an artifact from the project
        let foo_project_artifact_ref = brioche_core::project::artifact::create_project_artifact(
            &mut *brioche.write().await,
            foo_ref,
        )
        .await
        .expect("failed to create foo project artifact");

        // Read the current `project.bri` file from the current blob to
        // validate it exists at the path we expect
        let artifact_project_bri_path = format!("{foo_hash}/project.bri");
        let previous_project_bri_ref = brioche_test_support::get_recipe_within(
            &brioche,
            foo_project_artifact_ref,
            &artifact_project_bri_path,
        )
        .await;
        let _previous_project_bri_content =
            brioche_test_support::read_file_recipe_content(&brioche, previous_project_bri_ref);

        // Change the artifact so that the project hash no longer matches
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
        let new_foo_artifact = brioche_core::recipe::build::ArtifactBuilder::from_artifact(
            &*brioche.read().await,
            foo_project_artifact_ref,
        )
        .unwrap();
        let mut new_foo_artifact = Some(new_foo_artifact);
        brioche_core::recipe::build::insert_or_replace_in_artifact(
            &mut new_foo_artifact,
            &brioche_test_support::artifact_path(&artifact_project_bri_path),
            brioche_core::recipe::build::ArtifactBuilder::File {
                executable: false,
                content_blob: new_project_bri_blob,
                resources: Box::new(None),
            },
        )
        .unwrap();
        let new_foo_artifact = new_foo_artifact.unwrap();
        let new_foo_artifact_ref = brioche_core::recipe::build::build_artifact(
            &mut *brioche.write().await,
            &new_foo_artifact,
        )
        .unwrap();
        let new_foo_artifact_hash = brioche_core::recipe::hash::hash_recipe(
            &mut *brioche.write().await,
            new_foo_artifact_ref,
        );

        // Publish the artifact to the cache with the (incorrect) project hash
        brioche_core::cache::save_artifact(&mut *brioche.write().await, new_foo_artifact_ref)
            .await
            .unwrap();
        brioche_core::cache::save_project_artifact_hash(
            &mut *brioche.write().await,
            foo_hash,
            new_foo_artifact_hash,
        )
        .await
        .unwrap();

        foo_hash
    };

    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;

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
    let _project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::ProjectHashMismatch {
            expected_hash,
            actual_hash
        }] if *expected_hash == foo_hash && *actual_hash != foo_hash
    );

    mock_foo_latest.assert_async().await;
}

#[tokio::test]
async fn test_project_load_local_registry_dep_invalid_hash() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {
                        name: "foo"
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

    // Edit foo locally so its hash no longer matches!
    tokio::fs::write(
        foo_path.join("project.bri"),
        r#"
            export const project = {
                name: "bar"
            };
        "#,
    )
    .await
    .unwrap();

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

    let project_ref =
        brioche_test_support::load_project_ignoring_issues(&brioche, &project_dir).await;

    let brioche = &*brioche.read().await;
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::ProjectHashMismatch {
            expected_hash,
            actual_hash
        }] if *expected_hash == foo_hash && *actual_hash != foo_hash);

    let project_deps = brioche_core::project::get_dependencies(brioche, project_ref);
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::project::get_specifier(brioche, foo_ref);
    let foo_deps = brioche_core::project::get_dependencies(brioche, foo_ref);
    let foo_local_path = brioche_core::project::local_project_path(brioche, foo_ref)
        .to_system_path()
        .unwrap();
    assert_eq!(foo_specifier, ProjectSpecifier::Hash(foo_hash));
    assert_eq!(foo_local_path, foo_path);
    assert_eq!(
        foo_deps.len(),
        0,
        "expected to get 0 dependencies for foo, got: {foo_deps:#?}"
    );

    mock_foo_latest.assert_async().await;
}
