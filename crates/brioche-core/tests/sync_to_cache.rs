use std::sync::Arc;

use brioche_core::{
    Brioche,
    bake::BakeScope,
    cache::CacheClient,
    recipe::{Recipe, WithMeta},
    registry::RegistryClient,
};
use brioche_test_support::tpl;

#[tokio::test]
async fn test_sync_to_cache_process_and_complete_process() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, context) = brioche_test_with_cache(cache.clone(), true).await;

    // Create a process recipe and an equivalent complete_process recipe
    let process_recipe = brioche_core::recipe::ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("dummy_recipe")],
        platform: brioche_core::platform::Platform::X86_64Linux,
        ..brioche_test_support::default_process_x86_64_linux()
    };
    let process_recipe_hash = Recipe::Process(process_recipe.clone()).hash();
    let complete_process_recipe: brioche_core::recipe::CompleteProcessRecipe =
        process_recipe.clone().try_into()?;
    let complete_process_recipe_hash =
        Recipe::CompleteProcess(complete_process_recipe.clone()).hash();

    // Create a mocked output for the complete_process recipe
    let dummy_blob = brioche_test_support::blob(&brioche, "dummy value").await;
    let mocked_output = brioche_test_support::file(dummy_blob, false);
    brioche_core::cache::save_artifact(&brioche, mocked_output.clone()).await?;
    brioche_core::cache::save_bake(&brioche, complete_process_recipe_hash, mocked_output.hash())
        .await?;

    // Create a dummy project that we can associate the baked output with
    let (_, project_hash, _) = context
        .temp_project(|path| async move {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    // dummy project
                    export const project = {};
                "#,
            )
            .await
            .unwrap();
        })
        .await;

    // Bake the process recipe (and tie the result to our dummy project)
    let output_artifact = brioche_core::bake::bake(
        &brioche,
        WithMeta::without_meta(Recipe::Process(process_recipe.clone())),
        &BakeScope::Project {
            project_hash,
            export: "default".to_string(),
        },
    )
    .await?;

    // Ensure that we got the mock back
    assert_eq!(output_artifact.value, mocked_output);

    // Sync the project to the registry
    brioche_core::sync::sync_project(&brioche, project_hash, "default").await?;

    // Ensure the process recipe was saved in the cache with the mocked output
    let cached_output = brioche_core::cache::load_bake(&brioche, process_recipe_hash).await?;
    assert_eq!(cached_output, Some(mocked_output.hash()));

    Ok(())
}

async fn brioche_test_with_cache(
    store: Arc<dyn object_store::ObjectStore>,
    writable: bool,
) -> (Brioche, brioche_test_support::TestContext) {
    brioche_test_support::brioche_test_with(move |builder| {
        builder
            .cache_client(CacheClient {
                store: Some(store),
                writable,
                ..Default::default()
            })
            .registry_client(RegistryClient::disabled())
    })
    .await
}
