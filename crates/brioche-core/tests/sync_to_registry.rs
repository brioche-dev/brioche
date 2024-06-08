use brioche_core::{
    bake::BakeScope,
    recipe::{Recipe, WithMeta},
};
use brioche_test::tpl;

mod brioche_test;

#[tokio::test]
async fn test_sync_to_registry_process_and_complete_process() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

    // Create a process recipe and an equivalent complete_process recipe
    let process_recipe = brioche_core::recipe::ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("dummy_recipe")],
        platform: brioche_core::platform::Platform::X86_64Linux,
        ..brioche_test::default_process()
    };
    let process_recipe_hash = Recipe::Process(process_recipe.clone()).hash();
    let complete_process_recipe: brioche_core::recipe::CompleteProcessRecipe =
        process_recipe.clone().try_into()?;
    let complete_process_recipe_hash =
        Recipe::CompleteProcess(complete_process_recipe.clone()).hash();

    // Create a mocked output for the complete_process recipe
    let dummy_blob = brioche_test::blob(&brioche, "dummy value").await;
    let mocked_output = brioche_test::file(dummy_blob, false);
    brioche_test::mock_bake(
        &brioche,
        &Recipe::CompleteProcess(complete_process_recipe.clone()),
        &mocked_output,
    )
    .await;

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

    // Create mocks indicating the registry already has all the blobs
    // and recipes, but no bakes
    let mut registry_mocks = vec![];
    registry_mocks.push(
        context
            .registry_server
            .mock(
                "POST",
                &*format!("/v0/known-blobs?brioche={}", brioche_core::VERSION),
            )
            .with_body(serde_json::to_string(&[dummy_blob])?)
            .create(),
    );
    registry_mocks.push(
        context
            .registry_server
            .mock(
                "POST",
                &*format!("/v0/known-recipes?brioche={}", brioche_core::VERSION),
            )
            .with_body(serde_json::to_string(&[
                process_recipe_hash,
                complete_process_recipe_hash,
                mocked_output.hash(),
            ])?)
            .create(),
    );
    registry_mocks.push(
        context
            .registry_server
            .mock(
                "POST",
                &*format!("/v0/known-bakes?brioche={}", brioche_core::VERSION),
            )
            .with_body("[]")
            .create(),
    );

    // Create a mock to ensure that the result for complete_process
    // gets created
    registry_mocks.push(
        context
            .registry_server
            .mock(
                "POST",
                &*format!(
                    "/v0/recipes/{}/bake?brioche={}",
                    complete_process_recipe_hash,
                    brioche_core::VERSION
                ),
            )
            .with_body(serde_json::to_string(
                &brioche_core::registry::CreateBakeResponse {
                    canonical_output_hash: mocked_output.hash(),
                },
            )?)
            .create(),
    );

    // Sync the project to the registry
    brioche_core::sync::sync_project(&brioche, project_hash, "default").await?;

    // Ensure all the mocks got called as expected
    for registry_mock in registry_mocks {
        registry_mock.assert_async().await;
    }

    Ok(())
}
