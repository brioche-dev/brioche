#![allow(clippy::similar_names)]

use assert_matches::assert_matches;
use brioche_core::projects::{ProjectIssue, ProjectSpecifier, load::LoadModuleError};
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

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], &[]);

    let dependencies = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert!(dependencies.is_empty());
}

#[tokio::test]
async fn test_project_load_simple_no_definition() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context.write_file("myproject/project.bri", r"").await;

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], &[]);

    let dependencies = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert!(dependencies.is_empty());
}

#[tokio::test]
async fn test_project_load_workspace_dep() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::projects::WorkspaceDefinition {
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

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], &[]);

    let dependencies = brioche_core::projects::get_dependencies(&brioche, project_ref).await;

    let foo_specifier = brioche_core::projects::get_specifier(&brioche, dependencies["foo"]).await;
    assert_eq!(
        foo_specifier,
        brioche_test_support::project_specifier_for_path(&workspace_foo_dir)
    );

    let foo_dependencies =
        brioche_core::projects::get_dependencies(&brioche, dependencies["foo"]).await;
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

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], []);

    let project_deps = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert_eq!(
        project_deps.len(),
        2,
        "expected to get 2 project dependencies, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::projects::get_specifier(&brioche, foo_ref).await;
    let foo_deps = brioche_core::projects::get_dependencies(&brioche, foo_ref).await;
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
    let bar_specifier = brioche_core::projects::get_specifier(&brioche, bar_ref).await;
    let bar_deps = brioche_core::projects::get_dependencies(&brioche, bar_ref).await;
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
    let baz_specifier = brioche_core::projects::get_specifier(&brioche, baz_ref).await;
    let baz_deps = brioche_core::projects::get_dependencies(&brioche, baz_ref).await;
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

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], []);

    let project_deps = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::projects::get_specifier(&brioche, foo_ref).await;
    let foo_deps = brioche_core::projects::get_dependencies(&brioche, foo_ref).await;
    let foo_local_path = brioche_core::projects::local_project_path(&brioche, foo_ref)
        .await
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

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], []);

    let project_deps = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::projects::get_specifier(&brioche, foo_ref).await;
    let foo_deps = brioche_core::projects::get_dependencies(&brioche, foo_ref).await;
    let foo_local_path = brioche_core::projects::local_project_path(&brioche, foo_ref)
        .await
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

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], []);

    let project_deps = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::projects::get_specifier(&brioche, foo_ref).await;
    let foo_deps = brioche_core::projects::get_dependencies(&brioche, foo_ref).await;
    let foo_local_path = brioche_core::projects::local_project_path(&brioche, foo_ref)
        .await
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
async fn test_project_load_local_registry_dep_imported() -> anyhow::Result<()> {
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

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], []);

    let project_deps = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::projects::get_specifier(&brioche, foo_ref).await;
    let foo_deps = brioche_core::projects::get_dependencies(&brioche, foo_ref).await;
    let foo_local_path = brioche_core::projects::local_project_path(&brioche, foo_ref)
        .await
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

    Ok(())
}

#[tokio::test]
async fn test_project_load_remote_registry_dep() -> anyhow::Result<()> {
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

    let issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_matches!(&issues[..], []);

    let project_deps = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert_eq!(
        project_deps.len(),
        1,
        "expected to get 1 project dependency, got: {project_deps:#?}"
    );

    let foo_path = brioche
        .data_dir
        .join("projects")
        .join(foo_hash.to_string())
        .canonicalize()
        .unwrap();

    let foo_ref = project_deps["foo"];
    let foo_specifier = brioche_core::projects::get_specifier(&brioche, foo_ref).await;
    let foo_deps = brioche_core::projects::get_dependencies(&brioche, foo_ref).await;
    let foo_local_path = brioche_core::projects::local_project_path(&brioche, foo_ref)
        .await
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

    Ok(())
}

#[tokio::test]
async fn test_project_load_path_dep_not_found() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    let project_root_module = context
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

    let foo_root_module = context
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

    brioche_test_support::load_project(&brioche, &project_dir).await;

    let mut issues = brioche_core::projects::get_all_issues(&brioche).await;
    assert_eq!(issues.len(), 2, "expected 2 issues, got: {issues:#?}");

    let project_issue = brioche_test_support::take_where(&mut issues, |issue| {
        issue.location().is_some_and(|location| {
            location.path == brioche_test_support::absolute_path(&project_root_module)
        })
    });
    let foo_issue = brioche_test_support::take_where(&mut issues, |issue| {
        issue.location().is_some_and(|location| {
            location.path == brioche_test_support::absolute_path(&foo_root_module)
        })
    });
    assert!(issues.is_empty());

    let ProjectIssue::LoadModuleError {
        error: LoadModuleError::IoError { .. },
        path: project_issue_path,
        ..
    } = project_issue
    else {
        panic!("expected LoadModuleError::IoError, got: {project_issue:#?}");
    };
    let ProjectIssue::LoadModuleError {
        error: LoadModuleError::IoError { .. },
        path: foo_issue_path,
        ..
    } = foo_issue
    else {
        panic!("expected LoadModuleError::IoError, got: {foo_issue:#?}");
    };

    assert_eq!(
        project_issue_path,
        brioche_test_support::absolute_path(&not_found_1_dir).join_one("project.bri")
    );
    assert_eq!(
        foo_issue_path,
        brioche_test_support::absolute_path_nonexistent(&not_found_2_dir).join_one("project.bri")
    );
}
