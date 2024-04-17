use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use human_repr::HumanDuration;

use crate::{project::ProjectHash, references::ArtifactReferences, Brioche};

const RETRY_LIMIT: usize = 5;
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

pub async fn sync_project(
    brioche: &Brioche,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<()> {
    // TODO: Use reporter for logging in this function

    // Get all descendent resolves for the project/export

    let start_refs = std::time::Instant::now();

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

    println!(
        "Collected refs in {}",
        start_refs.elapsed().human_duration()
    );

    // Sync referenced blobs

    let start_blobs = std::time::Instant::now();

    let all_blobs = sync_references.blobs.iter().cloned().collect::<Vec<_>>();
    let known_blobs = brioche.registry_client.known_blobs(&all_blobs).await?;
    let new_blobs = all_blobs
        .into_iter()
        .filter(|blob_id| !known_blobs.contains(blob_id));

    futures::stream::iter(new_blobs)
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

    println!(
        "Finished syncing {} blobs in {}",
        sync_references.blobs.len(),
        start_blobs.elapsed().human_duration()
    );

    // Sync referenced artifacts

    let start_artifacts = std::time::Instant::now();

    let all_artifact_hashes = sync_references
        .artifacts
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let known_artifact_hashes = brioche
        .registry_client
        .known_artifacts(&all_artifact_hashes)
        .await?;
    let new_artifacts =
        sync_references
            .artifacts
            .clone()
            .into_iter()
            .filter_map(|(hash, artifact)| {
                if known_artifact_hashes.contains(&hash) {
                    None
                } else {
                    Some(artifact)
                }
            });

    futures::stream::iter(new_artifacts)
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

    println!(
        "Finished syncing {} artifacts in {}",
        sync_references.artifacts.len(),
        start_artifacts.elapsed().human_duration()
    );

    // Sync each resolve

    let start_resolves = std::time::Instant::now();

    let num_descendent_project_resolves = descendent_project_resolves.len();

    let all_resolves = descendent_project_resolves
        .into_iter()
        .map(|(input, output)| (input.hash(), output.hash()))
        .collect::<Vec<_>>();
    let known_resolves = brioche
        .registry_client
        .known_resolves(&all_resolves)
        .await?;
    let new_resolves = all_resolves
        .into_iter()
        .filter(|resolve| !known_resolves.contains(resolve));

    futures::stream::iter(new_resolves)
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
        "Finished syncing {num_descendent_project_resolves} resolves in {}",
        start_resolves.elapsed().human_duration()
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
