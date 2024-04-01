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
    assert!(project.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_repo_dep() -> anyhow::Result<()> {
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

    context
        .write_file(
            "brioche-repo/foo/project.bri",
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

    let foo_dep_hash = project.dependencies["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    assert!(projects
        .local_paths(foo_dep_hash)
        .unwrap()
        .contains(&brioche.repo_dir.join("foo")));
    assert!(foo_dep.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_workspace_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche::brioche::project::WorkspaceDefinition {
                members: vec!["./foo".parse()?],
            },
        )
        .await;

    let workspace_foo_dir = context.mkdir("myworkspace/foo").await;
    context
        .write_file(
            "myworkspace/foo/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;

    let repo_foo_dir = context.mkdir("brioche-repo/foo").await;
    context
        .write_file(
            "brioche-repo/foo/project.bri",
            r#"
                // repo foo
                export const project = {};
            "#,
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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
    let project = projects.project(project_hash).unwrap();
    assert!(projects
        .local_paths(project_hash)
        .unwrap()
        .contains(&project_dir));

    // "foo" from the workspace should take precedence over the repo dep
    let foo_dep_hash = project.dependencies["foo"];
    let foo_dep = projects.project(foo_dep_hash).unwrap();
    eprintln!("{:?}", projects.local_paths(foo_dep_hash));
    assert!(projects
        .local_paths(foo_dep_hash)
        .unwrap()
        .contains(&workspace_foo_dir));
    assert!(!projects
        .local_paths(foo_dep_hash)
        .unwrap()
        .contains(&repo_foo_dir));
    assert!(foo_dep.dependencies.is_empty());

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

    let dep_project_hash = project.dependencies["depproject"];
    let dep_project = projects.project(dep_project_hash).unwrap();
    assert!(projects
        .local_paths(dep_project_hash)
        .unwrap()
        .contains(&dep_project_dir));
    assert!(dep_project.dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_project_load_complex() -> anyhow::Result<()> {
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

    context
        .write_file(
            "brioche-repo/foo/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        bar: "*",
                    },
                };
            "#,
        )
        .await;

    context
        .write_file(
            "brioche-repo/bar/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &main_project_dir).await?;
    let project = projects.project(project_hash).unwrap();

    let main_dep_project_hash = project.dependencies["depproject"];
    let main_dep_project = projects.project(main_dep_project_hash).unwrap();

    let main_foo_project_hash = project.dependencies["foo"];
    let main_foo_project = projects.project(main_foo_project_hash).unwrap();

    let main_dep_foo_project_hash = main_dep_project.dependencies["foo"];
    let main_dep_foo_project = projects.project(main_dep_foo_project_hash).unwrap();

    let main_foo_bar_project_hash = main_foo_project.dependencies["bar"];

    let main_dep_foo_bar_project_hash = main_dep_foo_project.dependencies["bar"];

    assert!(projects
        .local_paths(main_dep_project_hash)
        .unwrap()
        .contains(&dep_project_dir));
    assert!(projects
        .local_paths(main_foo_project_hash)
        .unwrap()
        .contains(&brioche.repo_dir.join("foo")));
    assert!(projects
        .local_paths(main_dep_foo_project_hash)
        .unwrap()
        .contains(&brioche.repo_dir.join("foo")));
    assert!(projects
        .local_paths(main_foo_bar_project_hash)
        .unwrap()
        .contains(&brioche.repo_dir.join("bar")));
    assert!(projects
        .local_paths(main_dep_foo_bar_project_hash)
        .unwrap()
        .contains(&brioche.repo_dir.join("bar")));

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
async fn test_project_load_repo_dep_not_found() -> anyhow::Result<()> {
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

    // project.bri does not exist
    let _repo_foo_dir = context.mkdir("brioche-repo/foo").await;

    let result = brioche_test::load_project(&brioche, &project_dir)
        .await
        .map(|_| ());

    assert_matches!(result, Err(_));

    Ok(())
}
