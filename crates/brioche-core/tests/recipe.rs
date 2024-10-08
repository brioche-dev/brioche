use std::collections::HashMap;

use assert_matches::assert_matches;
use brioche_core::recipe::{Recipe, WithMeta};

#[tokio::test]
async fn test_recipe_get_nonexistent() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let file = brioche_core::recipe::Recipe::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche_core::recipe::Directory::default().into(),
        )),
    };

    let result = brioche_core::recipe::get_recipe(&brioche, file.hash()).await;
    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_recipe_save_none() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let new_artifacts = brioche_core::recipe::save_recipes(&brioche, [] as [Recipe; 0]).await?;
    assert_eq!(new_artifacts, 0);

    Ok(())
}

#[tokio::test]
async fn test_recipe_save_new() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let file = brioche_core::recipe::Recipe::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche_core::recipe::Directory::default().into(),
        )),
    };

    let new_artifacts = brioche_core::recipe::save_recipes(&brioche, [file.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let result = brioche_core::recipe::get_recipe(&brioche, file.hash()).await?;
    assert_eq!(file, result);

    Ok(())
}

#[tokio::test]
async fn test_recipe_save_repeat() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let file = brioche_core::recipe::Recipe::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche_core::recipe::Directory::default().into(),
        )),
    };

    let new_artifacts =
        brioche_core::recipe::save_recipes(&brioche, [file.clone(), file.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let new_artifacts =
        brioche_core::recipe::save_recipes(&brioche, [file.clone(), file.clone()]).await?;
    assert_eq!(new_artifacts, 0);

    let result = brioche_core::recipe::get_recipe(&brioche, file.hash()).await?;
    assert_eq!(file, result);

    Ok(())
}

#[tokio::test]
async fn test_recipe_save_multiple() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test_support::brioche_test().await;

    let file_1 = brioche_core::recipe::Recipe::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche_core::recipe::Directory::default().into(),
        )),
    };

    let file_2 = brioche_core::recipe::Recipe::CreateFile {
        content: "bar".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche_core::recipe::Directory::default().into(),
        )),
    };

    let file_3 = brioche_core::recipe::Recipe::CreateFile {
        content: "baz".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche_core::recipe::Directory::default().into(),
        )),
    };

    let new_artifacts =
        brioche_core::recipe::save_recipes(&brioche, [file_1.clone(), file_2.clone()]).await?;
    assert_eq!(new_artifacts, 2);

    let new_artifacts =
        brioche_core::recipe::save_recipes(&brioche, [file_2.clone(), file_3.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let results =
        brioche_core::recipe::get_recipes(&brioche, [file_1.hash(), file_2.hash(), file_3.hash()])
            .await?;

    assert_eq!(
        results,
        HashMap::from_iter([
            (file_1.hash(), file_1),
            (file_2.hash(), file_2),
            (file_3.hash(), file_3)
        ])
    );

    Ok(())
}
