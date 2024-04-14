use assert_matches::assert_matches;

mod brioche_test;

#[tokio::test]
async fn test_registry_client_get_project() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    let (projects, project_hash, _) = context
        .temp_project(|path| async move {
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
    let project = projects.project(project_hash).unwrap();

    let mock = context
        .registry_server
        .mock("GET", &*format!("/v0/projects/{project_hash}"))
        .with_header("Content-Type", "application/json")
        .with_body(serde_json::to_string(&project).unwrap())
        .create();

    let registry_project = brioche.registry_client.get_project(project_hash).await?;
    assert_eq!(registry_project, *project);

    mock.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_registry_client_get_project_invalid_hash() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    let (projects, project_hash, _) = context
        .temp_project(|path| async move {
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
    let project = projects.project(project_hash).unwrap();

    let mut changed_project = (*project).clone();
    changed_project.definition.name = Some("evil".to_string());

    let mock = context
        .registry_server
        .mock("GET", &*format!("/v0/projects/{project_hash}"))
        .with_header("Content-Type", "application/json")
        .with_body(serde_json::to_string(&changed_project).unwrap())
        .create();

    let result = brioche.registry_client.get_project(project_hash).await;
    assert_matches!(result, Err(_));

    mock.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_registry_client_get_blob() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    let path = context.write_file("test.txt", "hello world!").await;
    let (file_id, contents) = brioche.vfs.load(&path).await?;

    let mock = context
        .registry_server
        .mock("GET", &*format!("/v0/blobs/{file_id}"))
        .with_header("Content-Type", "application/octet-stream")
        .with_body(&*contents)
        .create();

    let blob_id = file_id.as_blob_id()?;
    let registry_contents = brioche.registry_client.get_blob(blob_id).await?;
    assert_eq!(registry_contents, *contents);

    mock.assert_async().await;

    Ok(())
}

#[tokio::test]
async fn test_registry_client_get_blob_invalid_hash() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    let path = context.write_file("test.txt", "hello world!").await;
    let (file_id, _) = brioche.vfs.load(&path).await?;

    let mock = context
        .registry_server
        .mock("GET", &*format!("/v0/blobs/{file_id}"))
        .with_header("Content-Type", "application/octet-stream")
        .with_body("evil")
        .create();

    let blob_id = file_id.as_blob_id()?;
    let result = brioche.registry_client.get_blob(blob_id).await;
    assert_matches!(result, Err(_));

    mock.assert_async().await;

    Ok(())
}
