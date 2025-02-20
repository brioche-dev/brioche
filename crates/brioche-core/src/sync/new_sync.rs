use futures::{StreamExt as _, TryStreamExt as _};

use crate::{utils::DisplayDuration, Brioche};

use super::SyncBakesResults;

#[expect(clippy::print_stdout)]
pub async fn sync_bakes(
    brioche: &Brioche,
    bakes: Vec<(crate::recipe::Recipe, crate::recipe::Artifact)>,
    verbose: bool,
) -> anyhow::Result<SyncBakesResults> {
    let sync_start = std::time::Instant::now();

    // Sync all artifacts to cache first
    let artifacts = bakes
        .iter()
        .map(|(_, artifact)| artifact)
        .cloned()
        .collect::<Vec<_>>();
    let num_new_artifacts = futures::stream::iter(artifacts)
        .map(|artifact| {
            let brioche = brioche.clone();
            async move { crate::cache::save_artifact(&brioche, artifact).await }
        })
        .buffer_unordered(25)
        .try_fold(0, |mut sum, is_new_artifact| async move {
            if is_new_artifact {
                sum += 1
            }

            Ok(sum)
        })
        .await?;

    // Sync all bakes to cache
    let num_bakes = bakes.len();
    let num_new_bakes = futures::stream::iter(bakes)
        .map(|(recipe, artifact)| {
            let input_hash = recipe.hash();
            let output_hash = artifact.hash();
            let brioche = brioche.clone();
            async move { crate::cache::save_bake(&brioche, input_hash, output_hash).await }
        })
        .buffer_unordered(25)
        .try_fold(0, |mut sum, is_new_bake| async move {
            if is_new_bake {
                sum += 1
            }

            Ok(sum)
        })
        .await?;

    if verbose {
        println!(
            "Finished syncing {num_new_bakes} / {num_bakes} bakes to cache in {}",
            DisplayDuration(sync_start.elapsed())
        );
    }

    Ok(SyncBakesResults {
        num_new_blobs: 0,
        num_new_bakes,
        num_new_recipes: num_new_artifacts,
    })
}
