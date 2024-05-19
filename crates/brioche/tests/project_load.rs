use assert_matches::assert_matches;

mod brioche_test;

#[tokio::test]
async fn test_project_load_simple() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let project = projects.project(project_hash).unwrap();
    assert!(projects
        .local_paths(project_hash)
        .unwrap()
        .contains(&project_dir));
    assert_eq!(project.dependencies().count(), 0);

    Ok(())
}

#[tokio::test]
async fn test_project_load_simple_no_definition() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context.write_file("myproject/project.bri", r#""#).await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let project = projects.project(project_hash).unwrap();
    assert!(projects
        .local_paths(project_hash)
        .unwrap()
        .contains(&project_dir));
    assert_eq!(project.dependencies().count(), 0);

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_workspace_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche::project::WorkspaceDefinition {
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
        .local_registry_project(|path| async move {
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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
    let project = projects.project(project_hash).unwrap();
    assert!(projects
        .local_paths(project_hash)
        .unwrap()
        .contains(&project_dir));

    // "foo" from the workspace should take precedence over the repo dep
    let foo_dep_hash = project.dependency_hash("foo").unwrap();
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(projects
        .local_paths(foo_dep_hash)
        .unwrap()
        .contains(&workspace_foo_dir));
    assert!(!projects
        .local_paths(foo_dep_hash)
        .unwrap()
        .contains(&registry_foo_dir));
    assert_eq!(foo_dep.dependencies().count(), 0);

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_path_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &main_project_dir).await?;
    let project = projects.project(project_hash).unwrap();
    assert!(projects
        .local_paths(project_hash)
        .unwrap()
        .contains(&main_project_dir));

    let dep_project_hash = project.dependency_hash("depproject").unwrap();
    let dep_project = projects.project(dep_project_hash).unwrap();
    assert!(projects
        .local_paths(dep_project_hash)
        .unwrap()
        .contains(&dep_project_dir));
    assert_eq!(dep_project.dependencies().count(), 0);

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_local_registry_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    let (foo_hash, foo_path) = context
        .local_registry_project(|path| async move {
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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
    let project = projects.project(project_hash).unwrap();
    assert!(projects
        .local_paths(project_hash)
        .unwrap()
        .contains(&project_dir));

    let foo_dep_hash = project.dependency_hash("foo").unwrap();
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(projects
        .local_paths(foo_dep_hash)
        .unwrap()
        .contains(&foo_path));
    assert_eq!(foo_dep.dependencies().count(), 0);

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_registry_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    let foo_hash = context
        .remote_registry_project(|path| async move {
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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
    let project = projects.project(project_hash).unwrap();
    assert!(projects
        .local_paths(project_hash)
        .unwrap()
        .contains(&project_dir));

    let foo_dep_hash = project.dependency_hash("foo").unwrap();
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    let foo_path = brioche.home.join("projects").join(foo_dep_hash.to_string());
    assert!(projects
        .local_paths(foo_dep_hash)
        .unwrap()
        .contains(&foo_path));
    assert_eq!(foo_dep.dependencies().count(), 0);

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_remote_workspace_registry_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche::project::WorkspaceDefinition {
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

    let projects = brioche::project::Projects::default();
    let bar_hash = projects
        .load(&brioche, &bar_dir, true)
        .await
        .expect("failed to load bar project");
    let foo_hash = projects
        .load(&brioche, &foo_dir, true)
        .await
        .expect("failed to load foo project");

    let bar_project = projects.project(bar_hash).expect("bar project not found");

    assert_eq!(bar_project.dependencies.get("foo"), Some(&foo_hash));

    let bar_listing = projects.export_listing(&brioche, bar_hash)?;
    let bar_mocks = context.mock_registry_listing(&bar_listing);
    for mock in bar_mocks {
        mock.create_async().await;
    }

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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
    let project = projects.project(project_hash).unwrap();

    let bar_dep_hash = project.dependency_hash("bar").unwrap();
    let bar_dep_project = projects.project(bar_dep_hash).unwrap();
    assert_eq!(bar_project, bar_dep_project);

    mock_bar_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_locked_registry_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    let bar_hash = context
        .remote_registry_project(|path| async move {
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
        .remote_registry_project(|path| async move {
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

    mock_bar_latest.assert_async().await;
    mock_bar_latest.remove_async().await;
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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
    let project = projects.project(project_hash).unwrap();
    projects.commit_dirty_lockfiles().await?;
    assert!(projects
        .local_paths(project_hash)
        .unwrap()
        .contains(&project_dir));

    let project_lockfile_path = project_dir.join("brioche.lock");
    assert!(tokio::fs::try_exists(&project_lockfile_path).await?);

    let project_lockfile_contents = tokio::fs::read_to_string(&project_lockfile_path).await?;
    let project_lockfile: brioche::project::Lockfile =
        serde_json::from_str(&project_lockfile_contents)?;

    let foo_lockfile_dep_hash = project.dependencies["foo"];
    assert_eq!(foo_lockfile_dep_hash, foo_hash);

    // "foo" should be in the lockfile
    assert!(project.dependencies.contains_key("foo"));
    assert!(project_lockfile.dependencies.contains_key("foo"));

    mock_foo_latest.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_project_load_complex() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

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
        .local_registry_project(|path| async move {
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
        .local_registry_project(|path| async move {
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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &main_project_dir).await?;
    let project = projects.project(project_hash).unwrap();

    let main_dep_project_hash = project.dependency_hash("depproject").unwrap();
    let main_dep_project = projects.project(main_dep_project_hash).unwrap();

    let main_foo_project_hash = project.dependency_hash("foo").unwrap();
    let main_foo_project = projects.project(main_foo_project_hash).unwrap();

    let main_dep_foo_project_hash = main_dep_project.dependency_hash("foo").unwrap();
    let main_dep_foo_project = projects.project(main_dep_foo_project_hash).unwrap();

    let main_foo_bar_project_hash = main_foo_project.dependency_hash("bar").unwrap();

    let main_dep_foo_bar_project_hash = main_dep_foo_project.dependency_hash("bar").unwrap();

    assert!(projects
        .local_paths(main_dep_project_hash)
        .unwrap()
        .contains(&dep_project_dir));
    assert!(projects
        .local_paths(main_foo_project_hash)
        .unwrap()
        .contains(&foo_path));
    assert!(projects
        .local_paths(main_dep_foo_project_hash)
        .unwrap()
        .contains(&foo_path));
    assert!(projects
        .local_paths(main_foo_bar_project_hash)
        .unwrap()
        .contains(&bar_path));
    assert!(projects
        .local_paths(main_dep_foo_bar_project_hash)
        .unwrap()
        .contains(&bar_path));

    assert_eq!(main_foo_project_hash, main_dep_foo_project_hash);
    assert_eq!(main_foo_bar_project_hash, main_dep_foo_bar_project_hash);

    Ok(())
}

#[tokio::test]
async fn test_project_load_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    // project.bri does not exist
    let project_dir = context.mkdir("myproject").await;

    let result = brioche_test::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_path_dep_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let result = brioche_test::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_project_load_dep_not_found() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let result = brioche_test::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}
