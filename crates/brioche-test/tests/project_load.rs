use std::sync::Arc;

use assert_matches::assert_matches;
use brioche_core::{Brioche, projects::ProjectSpecifier};
use brioche_test::TestContext;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn test_project_load_simple() {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r"
                export const project = {};
            ",
        )
        .await;

    let project_ref = brioche_test::load_project(&brioche, &project_dir).await;

    let dependencies = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert!(dependencies.is_empty());
}

#[tokio::test]
async fn test_project_load_simple_no_definition() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context.write_file("myproject/project.bri", r"").await;

    let project_ref = brioche_test::load_project(&brioche, &project_dir).await;

    let dependencies = brioche_core::projects::get_dependencies(&brioche, project_ref).await;
    assert!(dependencies.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_project_load_with_workspace_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let project_ref = brioche_test::load_project(&brioche, &project_dir).await;
    let dependencies = brioche_core::projects::get_dependencies(&brioche, project_ref).await;

    let foo_specifier = brioche_core::projects::get_specifier(&brioche, dependencies["foo"]).await;
    assert_eq!(
        foo_specifier,
        brioche_test::project_specifier_for_path(&brioche, &workspace_foo_dir)
    );

    let foo_dependencies =
        brioche_core::projects::get_dependencies(&brioche, dependencies["foo"]).await;
    assert!(foo_dependencies.is_empty());

    Ok(())
}
