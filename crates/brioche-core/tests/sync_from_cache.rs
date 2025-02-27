use std::sync::Arc;

use brioche_core::{
    Brioche,
    bake::BakeScope,
    cache::CacheClient,
    recipe::{Recipe, WithMeta},
};
use brioche_test_support::tpl;

#[tokio::test]
async fn test_sync_from_cache_complete_process() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, _context) = brioche_test_with_cache(cache, true).await;

    // Create a process recipe and an equivalent complete_process recipe
    let process_recipe = brioche_core::recipe::ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("dummy_recipe")],
        platform: brioche_core::platform::Platform::X86_64Linux,
        ..brioche_test_support::default_process_x86_64_linux()
    };
    let complete_process_recipe: brioche_core::recipe::CompleteProcessRecipe =
        process_recipe.clone().try_into()?;
    let complete_process_recipe_hash =
        Recipe::CompleteProcess(complete_process_recipe.clone()).hash();

    let dummy_blob = brioche_test_support::blob(&brioche, "dummy value").await;
    let mocked_output = brioche_test_support::file(dummy_blob, false);

    // Save the mocked output to the cache
    brioche_core::cache::save_artifact(&brioche, mocked_output.clone()).await?;

    // Save a bake in the cache for the complete_process recipe
    brioche_core::cache::save_bake(&brioche, complete_process_recipe_hash, mocked_output.hash())
        .await?;

    // Bake the process recipe, which should be a cache hit to the registry
    let output_artifact = brioche_core::bake::bake(
        &brioche,
        WithMeta::without_meta(Recipe::Process(process_recipe.clone())),
        &BakeScope::Anonymous,
    )
    .await?;

    // Ensure that we got the mock back
    assert_eq!(output_artifact.value, mocked_output);

    Ok(())
}

#[tokio::test]
async fn test_sync_from_cache_process() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, _context) = brioche_test_with_cache(cache, true).await;

    // Create a process recipe
    let process_recipe = brioche_core::recipe::ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("dummy_recipe")],
        platform: brioche_core::platform::Platform::X86_64Linux,
        ..brioche_test_support::default_process_x86_64_linux()
    };
    let process_recipe_hash = Recipe::Process(process_recipe.clone()).hash();

    let dummy_blob = brioche_test_support::blob(&brioche, "dummy value").await;
    let mocked_output = brioche_test_support::file(dummy_blob, false);

    // Save the mocked output to the cache
    brioche_core::cache::save_artifact(&brioche, mocked_output.clone()).await?;

    // Save a bake in the cache for the process recipe
    brioche_core::cache::save_bake(&brioche, process_recipe_hash, mocked_output.hash()).await?;

    // Bake the process recipe, which should be a cache hit
    let output_artifact = brioche_core::bake::bake(
        &brioche,
        WithMeta::without_meta(Recipe::Process(process_recipe.clone())),
        &BakeScope::Anonymous,
    )
    .await?;

    // Ensure that we got the mock back
    assert_eq!(output_artifact.value, mocked_output);

    Ok(())
}

async fn brioche_test_with_cache(
    store: Arc<dyn object_store::ObjectStore>,
    writable: bool,
) -> (Brioche, brioche_test_support::TestContext) {
    brioche_test_support::brioche_test_with(move |builder| {
        builder.cache_client(CacheClient {
            store: Some(store),
            writable,
            ..Default::default()
        })
    })
    .await
}
