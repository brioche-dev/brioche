use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};

use crate::{Brioche, project::ProjectHash, utils::DisplayDuration};

pub async fn wait_for_in_progress_syncs(brioche: &Brioche) -> anyhow::Result<SyncBakesResults> {
    let (sync_complete_tx, sync_complete_rx) = tokio::sync::oneshot::channel();

    brioche
        .sync_tx
        .send(crate::SyncMessage::Flush {
            completed: sync_complete_tx,
        })
        .await
        .context("failed to send flush sync message")?;
    let result = sync_complete_rx
        .await
        .context("failed to receive flush sync response")?;

    Ok(result)
}

pub async fn sync_project(
    brioche: &Brioche,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<()> {
    // Get all descendent bakes for the project/export

    let descendent_project_bakes =
        crate::references::descendent_project_bakes(brioche, project_hash, export).await?;

    // Sync all descendent bakes

    sync_bakes(brioche, descendent_project_bakes, true).await?;

    Ok(())
}

#[expect(clippy::print_stdout)]
pub async fn sync_bakes(
    brioche: &Brioche,
    bakes: Vec<(crate::recipe::Recipe, crate::recipe::Artifact)>,
    verbose: bool,
) -> anyhow::Result<SyncBakesResults> {
    if !brioche.cache_client.writable {
        anyhow::bail!("cannot sync: cache is read-only");
    }

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
                sum += 1;
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
                sum += 1;
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
        num_new_bakes,
        num_new_recipes: num_new_artifacts,
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncBakesResults {
    pub num_new_recipes: usize,
    pub num_new_bakes: usize,
}

impl SyncBakesResults {
    pub const fn merge(&mut self, other: Self) {
        self.num_new_recipes += other.num_new_recipes;
        self.num_new_bakes += other.num_new_bakes;
    }
}
