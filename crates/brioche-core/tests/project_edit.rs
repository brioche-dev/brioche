use brioche_core::project::edit::edit_project;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn test_project_edit_empty() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let foo_dir = context.mkdir("foo").await;
    let foo_module = indoc::indoc! {r#"
        export const project = {
          name: "foo",
          version: "0.1.0",
        };
    "#};
    let foo_module_path = context.write_file("foo/project.bri", foo_module).await;

    let (_, foo_hash_initial) = brioche_test_support::load_project(&brioche, &foo_dir).await?;

    let did_change = edit_project(
        &brioche.vfs,
        &foo_dir,
        brioche_core::project::edit::ProjectChanges::default(),
    )
    .await?;

    let (_, foo_hash_final) = brioche_test_support::load_project(&brioche, &foo_dir).await?;

    assert!(!did_change);
    assert_eq!(
        tokio::fs::read_to_string(&foo_module_path).await?,
        foo_module
    );
    assert_eq!(foo_hash_initial, foo_hash_final);

    Ok(())
}

#[tokio::test]
async fn test_project_edit_identical() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let foo_dir = context.mkdir("foo").await;
    let foo_module = indoc::indoc! {r#"
        export const project = {
          name: "foo",
          version: "0.1.0",
        };
    "#};
    let foo_module_path = context.write_file("foo/project.bri", foo_module).await;

    let (_, foo_hash_initial) = brioche_test_support::load_project(&brioche, &foo_dir).await?;

    let did_change = edit_project(
        &brioche.vfs,
        &foo_dir,
        brioche_core::project::edit::ProjectChanges {
            project_definition: Some(serde_json::json!({
                "name": "foo",
                "version": "0.1.0",
            })),
        },
    )
    .await?;

    let (_, foo_hash_final) = brioche_test_support::load_project(&brioche, &foo_dir).await?;

    assert!(!did_change);
    assert_eq!(
        tokio::fs::read_to_string(&foo_module_path).await?,
        foo_module
    );
    assert_eq!(foo_hash_initial, foo_hash_final);

    Ok(())
}

#[tokio::test]
async fn test_project_edit_edited() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let foo_dir = context.mkdir("foo").await;
    let foo_module = indoc::indoc! {r#"
        export const project = {
          name: "foo",
          version: "0.1.0",
        };
    "#};
    let foo_module_path = context.write_file("foo/project.bri", foo_module).await;

    let (_, foo_hash_initial) = brioche_test_support::load_project(&brioche, &foo_dir).await?;

    let did_change = edit_project(
        &brioche.vfs,
        &foo_dir,
        brioche_core::project::edit::ProjectChanges {
            project_definition: Some(serde_json::json!({
                "name": "foo",
                "version": "0.2.0",
            })),
        },
    )
    .await?;

    let (_, foo_hash_final) = brioche_test_support::load_project(&brioche, &foo_dir).await?;

    let foo_module_updated = indoc::indoc! {r#"
        export const project = {
          name: "foo",
          version: "0.2.0",
        };
    "#};
    assert!(did_change);
    assert_eq!(
        tokio::fs::read_to_string(&foo_module_path).await?,
        foo_module_updated
    );
    assert_ne!(foo_hash_initial, foo_hash_final);

    Ok(())
}

#[tokio::test]
async fn test_project_edit_preserves_order_and_extra_values() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let foo_dir = context.mkdir("foo").await;
    let foo_module = indoc::indoc! {r#"
        export const project = {
          name: "foo",
          version: "0.1.0",
        };
    "#};
    let foo_module_path = context.write_file("foo/project.bri", foo_module).await;

    let (_, foo_hash_initial) = brioche_test_support::load_project(&brioche, &foo_dir).await?;

    let did_change = edit_project(
        &brioche.vfs,
        &foo_dir,
        brioche_core::project::edit::ProjectChanges {
            project_definition: Some(serde_json::json!({
                "version": "0.2.0",
                "name": "foo",
                "extra": {
                    "a": [1, 2],
                    "c": null,
                    "b": {},
                },
            })),
        },
    )
    .await?;

    let (_, foo_hash_final) = brioche_test_support::load_project(&brioche, &foo_dir).await?;

    let foo_module_updated = indoc::indoc! {r#"
        export const project = {
          version: "0.2.0",
          name: "foo",
          extra: {
            a: [1, 2],
            c: null,
            b: {},
          },
        };
    "#};
    assert!(did_change);
    assert_eq!(
        tokio::fs::read_to_string(&foo_module_path).await?,
        foo_module_updated
    );
    assert_ne!(foo_hash_initial, foo_hash_final);

    Ok(())
}
