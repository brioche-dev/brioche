use std::collections::HashMap;

use anyhow::Context as _;
use sqlx::Acquire as _;

use crate::{
    artifact::{CompleteArtifact, LazyArtifact},
    project::ProjectHash,
    references::ArtifactReferences,
    Brioche,
};

pub async fn sync_project(brioche: &Brioche, project_hash: ProjectHash) -> anyhow::Result<()> {
    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    // Get the top-level project resolves
    let project_hash_value = project_hash.to_string();
    let project_artifacts = sqlx::query!(
        r#"
            SELECT
                resolves.input_hash,
                resolves.input_json,
                resolves.output_hash,
                resolves.output_json
            FROM resolves
            INNER JOIN project_resolves
                ON project_resolves.artifact_hash = resolves.input_hash
            WHERE project_resolves.project_hash = ?
        "#,
        project_hash_value,
    )
    .fetch_all(&mut *db_transaction)
    .await?;

    let project_artifacts = project_artifacts
        .into_iter()
        .map(|row| {
            let input_artifact: LazyArtifact =
                serde_json::from_str(&row.input_json).with_context(|| {
                    format!(
                        "failed to parse input artifact with hash {}",
                        row.input_hash
                    )
                })?;
            let output_artifact: CompleteArtifact = serde_json::from_str(&row.output_json)
                .with_context(|| {
                    format!(
                        "failed to parse output artifact with hash {}",
                        row.output_hash
                    )
                })?;

            anyhow::ensure!(
                input_artifact.hash().to_string() == row.input_hash,
                "input artifact hash mismatch: row hash {} does not match artifact hash {}",
                row.input_hash,
                input_artifact.hash()
            );
            anyhow::ensure!(
                output_artifact.hash().to_string() == row.output_hash,
                "output artifact hash mismatch: row hash {} does not match artifact hash {}",
                row.output_hash,
                output_artifact.hash()
            );

            Ok((input_artifact, output_artifact))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Collect all referenced artifacts/blobs from the top-level resolves

    let mut all_references = ArtifactReferences::default();
    for (input_artifact, output_artifact) in &project_artifacts {
        crate::references::artifact_references(
            brioche,
            &mut all_references,
            input_artifact.clone(),
        )
        .await?;
        crate::references::artifact_references(
            brioche,
            &mut all_references,
            output_artifact.clone().into(),
        )
        .await?;
    }

    // Filter to only the artifacts we want to sync

    let input_sync_artifacts = all_references
        .artifacts
        .values()
        .filter(|artifact| match artifact {
            LazyArtifact::CompleteProcess(_) | LazyArtifact::Download(_) => true,
            LazyArtifact::File { .. }
            | LazyArtifact::Directory(_)
            | LazyArtifact::Symlink { .. }
            | LazyArtifact::Unpack(_)
            | LazyArtifact::Process(_)
            | LazyArtifact::CreateFile { .. }
            | LazyArtifact::CreateDirectory(_)
            | LazyArtifact::Cast { .. }
            | LazyArtifact::Merge { .. }
            | LazyArtifact::Peel { .. }
            | LazyArtifact::Get { .. }
            | LazyArtifact::Insert { .. }
            | LazyArtifact::SetPermissions { .. }
            | LazyArtifact::Proxy(_) => false,
        })
        .cloned()
        .collect::<Vec<_>>();

    // Get the resolved output artifacts for the artifacts we want to sync

    let mut output_sync_artifacts = HashMap::new();
    for artifact in &input_sync_artifacts {
        let input_hash = artifact.hash();
        let input_hash_value = input_hash.to_string();

        let resolved = sqlx::query!(
            r#"
                SELECT output_hash, output_json
                FROM resolves
                WHERE input_hash = ?
            "#,
            input_hash_value,
        )
        .fetch_one(&mut *db_transaction)
        .await?;

        let output_artifact: CompleteArtifact = serde_json::from_str(&resolved.output_json)
            .with_context(|| {
                format!(
                    "failed to parse output artifact with hash {}",
                    resolved.output_hash
                )
            })?;
        anyhow::ensure!(
            output_artifact.hash().to_string() == resolved.output_hash,
            "output artifact hash mismatch: row hash {} does not match artifact hash {}",
            resolved.output_hash,
            output_artifact.hash()
        );

        output_sync_artifacts.insert(input_hash, output_artifact);
    }

    // Collect all references from the artifacts to sync. This gives us the
    // full set of things we want to sync

    let mut sync_references = ArtifactReferences::default();
    for artifact in &input_sync_artifacts {
        crate::references::artifact_references(brioche, &mut sync_references, artifact.clone())
            .await?;
    }
    for artifact in output_sync_artifacts.values() {
        crate::references::artifact_references(
            brioche,
            &mut sync_references,
            artifact.clone().into(),
        )
        .await?;
    }

    db_transaction.commit().await?;
    drop(db_conn);

    // Sync the artifacts

    for blob_id in &sync_references.blobs {
        let blob_path = crate::blob::blob_path(brioche, *blob_id);
        let blob = tokio::fs::File::open(&blob_path)
            .await
            .with_context(|| format!("blob {blob_id} not found"))?;

        brioche.registry_client.send_blob(*blob_id, blob).await?;
    }

    for artifact in sync_references.artifacts.values() {
        brioche.registry_client.create_artifact(artifact).await?;
    }

    for (input_hash, output_artifact) in output_sync_artifacts {
        let output_hash = output_artifact.hash();
        brioche
            .registry_client
            .create_resolve(input_hash, output_hash)
            .await?;
    }

    Ok(())
}
