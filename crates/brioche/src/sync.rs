use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};

use crate::{project::ProjectHash, references::ArtifactReferences, Brioche};

const RETRY_LIMIT: usize = 5;
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

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
                    retry(RETRY_LIMIT, RETRY_DELAY, || {
                        let brioche = brioche.clone();
                        let blob_path = blob_path.clone();
                        async move {
                            let blob = tokio::fs::File::open(&blob_path)
                                .await
                                .with_context(|| format!("failed to open blob {blob_id}"))?;

                            brioche.registry_client.send_blob(blob_id, blob).await?;

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

    // Sync referenced artifacts

    futures::stream::iter(sync_references.artifacts.clone().into_values())
        .map(Ok)
        .try_for_each_concurrent(Some(100), |artifact| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(async move {
                    retry(RETRY_LIMIT, RETRY_DELAY, || {
                        let brioche = brioche.clone();
                        let artifact = artifact.clone();
                        async move { brioche.registry_client.create_artifact(&artifact).await }
                    })
                    .await
                })
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
                    retry(RETRY_LIMIT, RETRY_DELAY, || {
                        let brioche = brioche.clone();
                        let input = input.clone();
                        let output = output.clone();
                        async move {
                            brioche
                                .registry_client
                                .create_resolve(input.hash(), output.hash())
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
