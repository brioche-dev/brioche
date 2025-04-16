use anyhow::Context as _;

use crate::{Brioche, project::ProjectHash, references::ProjectReferences};

mod legacy_sync;
mod new_sync;

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

pub async fn sync_bakes(
    brioche: &Brioche,
    bakes: Vec<(crate::recipe::Recipe, crate::recipe::Artifact)>,
    verbose: bool,
) -> anyhow::Result<SyncBakesResults> {
    let mut results: Option<SyncBakesResults> = None;

    if brioche.registry_client.can_sync() {
        let legacy_results = legacy_sync::sync_bakes(brioche, bakes.clone(), verbose).await?;
        results.get_or_insert_default().merge(legacy_results);
    }

    if brioche.cache_client.writable {
        let new_results = new_sync::sync_bakes(brioche, bakes, verbose).await?;
        results.get_or_insert_default().merge(new_results);
    }

    let results = results.context("cannot sync: cache is read-only")?;
    Ok(results)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncBakesResults {
    pub num_new_blobs: usize,
    pub num_new_recipes: usize,
    pub num_new_bakes: usize,
}

impl SyncBakesResults {
    pub const fn merge(&mut self, other: Self) {
        self.num_new_blobs += other.num_new_blobs;
        self.num_new_recipes += other.num_new_recipes;
        self.num_new_bakes += other.num_new_bakes;
    }
}

pub async fn legacy_sync_project_references(
    brioche: &Brioche,
    references: &ProjectReferences,
    verbose: bool,
) -> anyhow::Result<()> {
    if brioche.registry_client.can_sync() {
        legacy_sync::sync_project_references(brioche, references, verbose).await?;
    }

    Ok(())
}
