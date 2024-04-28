use std::collections::HashMap;

use assert_matches::assert_matches;
use brioche::recipe::{Recipe, WithMeta};

mod brioche_test;

#[tokio::test]
async fn test_recipe_get_nonexistent() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let file = brioche::recipe::Recipe::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::recipe::Directory::default().into(),
        )),
    };

    let result = brioche::recipe::get_recipe(&brioche, file.hash()).await;
    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_recipe_save_none() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let new_artifacts = brioche::recipe::save_recipes(&brioche, [] as [Recipe; 0]).await?;
    assert_eq!(new_artifacts, 0);

    Ok(())
}

#[tokio::test]
async fn test_recipe_save_new() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let file = brioche::recipe::Recipe::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::recipe::Directory::default().into(),
        )),
    };

    let new_artifacts = brioche::recipe::save_recipes(&brioche, [file.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let result = brioche::recipe::get_recipe(&brioche, file.hash()).await?;
    assert_eq!(file, result);

    Ok(())
}

#[tokio::test]
async fn test_recipe_save_repeat() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let file = brioche::recipe::Recipe::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::recipe::Directory::default().into(),
        )),
    };

    let new_artifacts =
        brioche::recipe::save_recipes(&brioche, [file.clone(), file.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let new_artifacts =
        brioche::recipe::save_recipes(&brioche, [file.clone(), file.clone()]).await?;
    assert_eq!(new_artifacts, 0);

    let result = brioche::recipe::get_recipe(&brioche, file.hash()).await?;
    assert_eq!(file, result);

    Ok(())
}

#[tokio::test]
async fn test_recipe_save_multiple() -> anyhow::Result<()> {
    let (brioche, _) = brioche_test::brioche_test().await;

    let file_1 = brioche::recipe::Recipe::CreateFile {
        content: "foo".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::recipe::Directory::default().into(),
        )),
    };

    let file_2 = brioche::recipe::Recipe::CreateFile {
        content: "bar".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::recipe::Directory::default().into(),
        )),
    };

    let file_3 = brioche::recipe::Recipe::CreateFile {
        content: "baz".into(),
        executable: false,
        resources: Box::new(WithMeta::without_meta(
            brioche::recipe::Directory::default().into(),
        )),
    };

    let new_artifacts =
        brioche::recipe::save_recipes(&brioche, [file_1.clone(), file_2.clone()]).await?;
    assert_eq!(new_artifacts, 2);

    let new_artifacts =
        brioche::recipe::save_recipes(&brioche, [file_2.clone(), file_3.clone()]).await?;
    assert_eq!(new_artifacts, 1);

    let results =
        brioche::recipe::get_recipes(&brioche, [file_1.hash(), file_2.hash(), file_3.hash()])
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
