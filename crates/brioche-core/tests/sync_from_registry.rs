use std::collections::HashSet;

use brioche_core::{
    bake::BakeScope,
    recipe::{Recipe, WithMeta},
};
use brioche_test::tpl;

mod brioche_test;

#[tokio::test]
async fn test_sync_from_registry_complete_process() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    // Create a process recipe and an equivalent complete_process recipe
    let process_recipe = brioche_core::recipe::ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("dummy_recipe")],
        platform: brioche_core::platform::Platform::X86_64Linux,
        ..brioche_test::default_process()
    };
    let complete_process_recipe: brioche_core::recipe::CompleteProcessRecipe =
        process_recipe.clone().try_into()?;
    let complete_process_recipe_hash =
        Recipe::CompleteProcess(complete_process_recipe.clone()).hash();

    let dummy_blob = brioche_test::blob(&brioche, "dummy value").await;
    let mocked_output = brioche_test::file(dummy_blob, false);

    // Mock the registry to return the output artifact for the complete_process
    // recipe. This should be the only registry request, needed since the
    // output artifact and blob have already been saved locally
    let mut registry_mocks = vec![];
    registry_mocks.push(
        context
            .registry_server
            .mock(
                "GET",
                &*format!(
                    "/v0/recipes/{}/bake?brioche={}",
                    complete_process_recipe_hash,
                    brioche_core::VERSION,
                ),
            )
            .with_body(serde_json::to_string(
                &brioche_core::registry::GetBakeResponse {
                    output_hash: mocked_output.hash(),
                    output_artifact: mocked_output.clone(),
                    referenced_recipes: HashSet::new(),
                    referenced_blobs: HashSet::new(),
                },
            )?)
            .create(),
    );

    // Bake the process recipe, which should be a cache hit to the registry
    let output_artifact = brioche_core::bake::bake(
        &brioche,
        WithMeta::without_meta(Recipe::Process(process_recipe.clone())),
        &BakeScope::Anonymous,
    )
    .await?;

    // Ensure that we got the mock back
    assert_eq!(output_artifact.value, mocked_output);

    // Ensure all the registry mocks got called as expected
    for registry_mock in registry_mocks {
        registry_mock.assert_async().await;
    }

    Ok(())
}

#[tokio::test]
async fn test_sync_from_registry_process() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    // Create a process recipe
    let process_recipe = brioche_core::recipe::ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("dummy_recipe")],
        platform: brioche_core::platform::Platform::X86_64Linux,
        ..brioche_test::default_process()
    };
    let process_recipe_hash = Recipe::Process(process_recipe.clone()).hash();

    let dummy_blob = brioche_test::blob(&brioche, "dummy value").await;
    let mocked_output = brioche_test::file(dummy_blob, false);

    // Mock the registry to return the output artifact for the process
    // recipe. This should be the only registry request, needed since the
    // output artifact and blob have already been saved locally
    let mut registry_mocks = vec![];
    registry_mocks.push(
        context
            .registry_server
            .mock(
                "GET",
                &*format!(
                    "/v0/recipes/{}/bake?brioche={}",
                    process_recipe_hash,
                    brioche_core::VERSION,
                ),
            )
            .with_body(serde_json::to_string(
                &brioche_core::registry::GetBakeResponse {
                    output_hash: mocked_output.hash(),
                    output_artifact: mocked_output.clone(),
                    referenced_recipes: HashSet::new(),
                    referenced_blobs: HashSet::new(),
                },
            )?)
            .create(),
    );

    // Bake the process recipe, which should be a cache hit to the registry
    let output_artifact = brioche_core::bake::bake(
        &brioche,
        WithMeta::without_meta(Recipe::Process(process_recipe.clone())),
        &BakeScope::Anonymous,
    )
    .await?;

    // Ensure that we got the mock back
    assert_eq!(output_artifact.value, mocked_output);

    // Ensure all the registry mocks got called as expected
    for registry_mock in registry_mocks {
        registry_mock.assert_async().await;
    }

    Ok(())
}
