use assert_matches::assert_matches;
use brioche::artifact::{LazyArtifact, WithMeta};

mod brioche_test;

#[tokio::test]
async fn test_artifact_get_nonexistent() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let file = brioche::artifact::LazyArtifact::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::artifact::Directory::default().into(),
        )),
    };

    let result = brioche::artifact::get_artifact(&brioche, file.hash()).await;
    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_artifact_save_none() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let new_artifacts =
        brioche::artifact::save_artifacts(&brioche, [] as [LazyArtifact; 0]).await?;
    assert_eq!(new_artifacts, 0);

    Ok(())
}

#[tokio::test]
async fn test_artifact_save_new() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let file = brioche::artifact::LazyArtifact::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::artifact::Directory::default().into(),
        )),
    };

    let new_artifacts = brioche::artifact::save_artifacts(&brioche, [file.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let result = brioche::artifact::get_artifact(&brioche, file.hash()).await?;
    assert_eq!(file, result);

    Ok(())
}

#[tokio::test]
async fn test_artifact_save_repeat() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let file = brioche::artifact::LazyArtifact::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::artifact::Directory::default().into(),
        )),
    };

    let new_artifacts =
        brioche::artifact::save_artifacts(&brioche, [file.clone(), file.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let new_artifacts =
        brioche::artifact::save_artifacts(&brioche, [file.clone(), file.clone()]).await?;
    assert_eq!(new_artifacts, 0);

    let result = brioche::artifact::get_artifact(&brioche, file.hash()).await?;
    assert_eq!(file, result);

    Ok(())
}

#[tokio::test]
async fn test_artifact_save_multiple() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let file_1 = brioche::artifact::LazyArtifact::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::artifact::Directory::default().into(),
        )),
    };

    let file_2 = brioche::artifact::LazyArtifact::CreateFile {
        content: "bar".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::artifact::Directory::default().into(),
        )),
    };

    let file_3 = brioche::artifact::LazyArtifact::CreateFile {
        content: "baz".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::artifact::Directory::default().into(),
        )),
    };

    let new_artifacts =
        brioche::artifact::save_artifacts(&brioche, [file_1.clone(), file_2.clone()]).await?;
    assert_eq!(new_artifacts, 2);

    let new_artifacts =
        brioche::artifact::save_artifacts(&brioche, [file_2.clone(), file_3.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let result_1 = brioche::artifact::get_artifact(&brioche, file_1.hash()).await?;
    assert_eq!(result_1, file_1);

    let result_2 = brioche::artifact::get_artifact(&brioche, file_2.hash()).await?;
    assert_eq!(result_2, file_2);

    let result_3 = brioche::artifact::get_artifact(&brioche, file_3.hash()).await?;
    assert_eq!(result_3, file_3);

    Ok(())
}
