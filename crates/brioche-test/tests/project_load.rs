use std::sync::Arc;

use assert_matches::assert_matches;
use brioche_core::Brioche;
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
