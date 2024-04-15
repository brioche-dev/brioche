use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};

use crate::{project::ProjectHash, references::ArtifactReferences, Brioche};

pub async fn sync_project(
    brioche: &Brioche,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<()> {
    // Get all descendent resolves for the project/export

    let descendent_project_resolves =
        crate::references::descendent_project_resolves(brioche, project_hash, export).await?;

    // Collect the references from each input/output artifact

    let mut sync_references = ArtifactReferences::default();
    for (input, output) in &descendent_project_resolves {
        crate::references::artifact_references(brioche, &mut sync_references, input.clone())
            .await?;
        crate::references::artifact_references(
            brioche,
            &mut sync_references,
            output.clone().into(),
        )
        .await?;
    }

    // Sync referenced blobs

    futures::stream::iter(sync_references.blobs.clone())
        .map(Ok)
        .try_for_each_concurrent(Some(100), |blob_id| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(async move {
                    let blob_path = crate::blob::blob_path(&brioche, blob_id).await?;
                    let blob = tokio::fs::File::open(&blob_path)
                        .await
                        .with_context(|| format!("blob {blob_id} not found"))?;

                    brioche.registry_client.send_blob(blob_id, blob).await?;

                    anyhow::Ok(())
                })
                .await??;

                anyhow::Ok(())
            }
        })
        .await?;

    // Sync referenced artifacts

    futures::stream::iter(sync_references.artifacts.clone().into_values())
        .map(Ok)
        .try_for_each_concurrent(Some(100), |artifact| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(
                    async move { brioche.registry_client.create_artifact(&artifact).await },
                )
                .await??;
                anyhow::Ok(())
            }
        })
        .await?;

    // Sync each resolve

    futures::stream::iter(descendent_project_resolves)
        .map(Ok)
        .try_for_each_concurrent(Some(100), |(input, output)| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(async move {
                    brioche
                        .registry_client
                        .create_resolve(input.hash(), output.hash())
                        .await
                })
                .await??;
                anyhow::Ok(())
            }
        })
        .await?;

    Ok(())
}
