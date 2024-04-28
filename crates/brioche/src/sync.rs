use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use human_repr::HumanDuration;

use crate::{project::ProjectHash, references::RecipeReferences, Brioche};

const RETRY_LIMIT: usize = 5;
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

pub async fn sync_project(
    brioche: &Brioche,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<()> {
    // TODO: Use reporter for logging in this function

    // Get all descendent bakes for the project/export

    let start_refs = std::time::Instant::now();

    let descendent_project_bakes =
        crate::references::descendent_project_bakes(brioche, project_hash, export).await?;

    // Collect the references from each input recipe/output artifact

    let mut sync_references = RecipeReferences::default();

    let recipe_hashes = descendent_project_bakes
        .iter()
        .flat_map(|(input, output)| [input.hash(), output.hash()]);
    crate::references::recipe_references(brioche, &mut sync_references, recipe_hashes).await?;

    let num_recipe_refs = sync_references.recipes.len();
    let num_blob_refs = sync_references.blobs.len();
    println!(
        "Collected refs in {} ({num_recipe_refs} recipes, {num_blob_refs} blobs)",
        start_refs.elapsed().human_duration()
    );

    // Sync referenced blobs

    let start_blobs = std::time::Instant::now();

    let all_blobs = sync_references.blobs.iter().cloned().collect::<Vec<_>>();
    let known_blobs = brioche.registry_client.known_blobs(&all_blobs).await?;
    let new_blobs = all_blobs
        .into_iter()
        .filter(|blob_hash| !known_blobs.contains(blob_hash));

    futures::stream::iter(new_blobs)
        .map(Ok)
        .try_for_each_concurrent(Some(100), |blob_hash| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(async move {
                    let blob_path = crate::blob::blob_path(&brioche, blob_hash).await?;
                    retry(RETRY_LIMIT, RETRY_DELAY, || {
                        let brioche = brioche.clone();
                        let blob_path = blob_path.clone();
                        async move {
                            let blob = tokio::fs::File::open(&blob_path)
                                .await
                                .with_context(|| format!("failed to open blob {blob_hash}"))?;

                            brioche.registry_client.send_blob(blob_hash, blob).await?;

                            anyhow::Ok(())
                        }
                    })
                    .await?;

                    anyhow::Ok(())
                })
                .await??;

                anyhow::Ok(())
            }
        })
        .await?;

    println!(
        "Finished syncing {} blobs in {}",
        sync_references.blobs.len(),
        start_blobs.elapsed().human_duration()
    );

    // Sync referenced recipes

    let start_recipes = std::time::Instant::now();

    let all_recipe_hashes = sync_references.recipes.keys().cloned().collect::<Vec<_>>();
    let known_recipe_hashes = brioche
        .registry_client
        .known_artifacts(&all_recipe_hashes)
        .await?;
    let new_recipes: Vec<_> = sync_references
        .recipes
        .clone()
        .into_iter()
        .filter_map(|(hash, recipe)| {
            if known_recipe_hashes.contains(&hash) {
                None
            } else {
                Some(recipe)
            }
        })
        .collect();

    brioche
        .registry_client
        .create_artifacts(&new_recipes)
        .await?;

    println!(
        "Finished syncing {} recipes in {}",
        sync_references.recipes.len(),
        start_recipes.elapsed().human_duration()
    );

    // Sync each baked recipe

    let start_bakes = std::time::Instant::now();

    let num_descendent_project_bakes = descendent_project_bakes.len();

    let all_bakes = descendent_project_bakes
        .into_iter()
        .map(|(input, output)| (input.hash(), output.hash()))
        .collect::<Vec<_>>();
    let known_bakes = brioche.registry_client.known_resolves(&all_bakes).await?;
    let new_bakes = all_bakes
        .into_iter()
        .filter(|bake| !known_bakes.contains(bake));

    futures::stream::iter(new_bakes)
        .map(Ok)
        .try_for_each_concurrent(Some(100), |(input_hash, output_hash)| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(async move {
                    retry(RETRY_LIMIT, RETRY_DELAY, || {
                        let brioche = brioche.clone();
                        async move {
                            brioche
                                .registry_client
                                .create_resolve(input_hash, output_hash)
                                .await
                        }
                    })
                    .await
                })
                .await??;
                anyhow::Ok(())
            }
        })
        .await?;

    println!(
        "Finished syncing {num_descendent_project_bakes} bakes in {}",
        start_bakes.elapsed().human_duration()
    );

    Ok(())
}

async fn retry<Fut, T, E>(
    mut attempts: usize,
    delay: std::time::Duration,
    mut f: impl FnMut() -> Fut,
) -> Result<T, E>
where
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    loop {
        let result = f().await;
        attempts = attempts.saturating_sub(1);

        match result {
            Ok(value) => {
                return Ok(value);
            }
            Err(error) if attempts == 0 => {
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(remaining_attempts = attempts, %error, "retrying after error");
                tokio::time::sleep(delay).await;
            }
        }
    }
}
