use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Context as _;
use sqlx::Acquire as _;

use crate::{
    artifact::{
        ArtifactHash, CompleteArtifact, CompleteProcessArtifact, CompleteProcessTemplateComponent,
        LazyArtifact, ProcessArtifact, ProcessTemplateComponent,
    },
    blob::BlobHash,
    project::ProjectHash,
    Brioche,
};

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArtifactReferences {
    pub blobs: HashSet<BlobHash>,
    pub artifacts: HashMap<ArtifactHash, LazyArtifact>,
}

pub async fn artifact_references(
    brioche: &Brioche,
    references: &mut ArtifactReferences,
    artifacts: impl IntoIterator<Item = ArtifactHash>,
) -> anyhow::Result<()> {
    let mut unvisited = VecDeque::from_iter(artifacts);

    loop {
        unvisited.retain(|artifact| !references.artifacts.contains_key(artifact));

        if unvisited.is_empty() {
            break;
        }

        let artifacts = crate::artifact::get_artifacts(brioche, unvisited.drain(..)).await?;

        for artifact in artifacts.values() {
            unvisited.extend(referenced_artifacts(artifact));
            references.blobs.extend(referenced_blobs(artifact));
        }

        references.artifacts.extend(artifacts);
    }

    Ok(())
}

pub fn referenced_blobs(artifact: &LazyArtifact) -> Vec<BlobHash> {
    match artifact {
        LazyArtifact::File { content_blob, .. } => vec![*content_blob],
        LazyArtifact::Directory(_)
        | LazyArtifact::Symlink { .. }
        | LazyArtifact::Download(_)
        | LazyArtifact::Unpack(_)
        | LazyArtifact::Process(_)
        | LazyArtifact::CompleteProcess(_)
        | LazyArtifact::CreateFile { .. }
        | LazyArtifact::CreateDirectory(_)
        | LazyArtifact::Cast { .. }
        | LazyArtifact::Merge { .. }
        | LazyArtifact::Peel { .. }
        | LazyArtifact::Get { .. }
        | LazyArtifact::Insert { .. }
        | LazyArtifact::SetPermissions { .. }
        | LazyArtifact::Proxy(_) => vec![],
    }
}

pub fn referenced_artifacts(artifact: &LazyArtifact) -> Vec<ArtifactHash> {
    match artifact {
        LazyArtifact::File {
            resources,
            content_blob: _,
            executable: _,
        } => referenced_artifacts(resources),
        LazyArtifact::Directory(directory) => directory
            .entry_hashes()
            .values()
            .map(|entry| entry.value)
            .collect(),
        LazyArtifact::Symlink { .. } => vec![],
        LazyArtifact::Download(_) => vec![],
        LazyArtifact::Unpack(unpack) => referenced_artifacts(&unpack.file),
        LazyArtifact::Process(process) => {
            let ProcessArtifact {
                command,
                args,
                env,
                work_dir,
                output_scaffold,
                platform: _,
            } = process;

            let templates = [command].into_iter().chain(args).chain(env.values());

            templates
                .flat_map(|template| &template.components)
                .flat_map(|component| match component {
                    ProcessTemplateComponent::Input { artifact } => referenced_artifacts(artifact),
                    ProcessTemplateComponent::Literal { .. }
                    | ProcessTemplateComponent::OutputPath
                    | ProcessTemplateComponent::ResourcesDir
                    | ProcessTemplateComponent::HomeDir
                    | ProcessTemplateComponent::WorkDir
                    | ProcessTemplateComponent::TempDir => vec![],
                })
                .chain(referenced_artifacts(work_dir))
                .chain(
                    output_scaffold
                        .iter()
                        .flat_map(|artifact| referenced_artifacts(artifact)),
                )
                .collect()
        }
        LazyArtifact::CompleteProcess(process) => {
            let CompleteProcessArtifact {
                command,
                args,
                env,
                work_dir,
                output_scaffold,
                platform: _,
            } = process;

            let work_dir = LazyArtifact::from(work_dir.clone());
            let output_scaffold = output_scaffold
                .as_ref()
                .map(|artifact| LazyArtifact::from((**artifact).clone()));

            let templates = [command].into_iter().chain(args).chain(env.values());

            templates
                .flat_map(|template| &template.components)
                .flat_map(|component| match component {
                    CompleteProcessTemplateComponent::Input { artifact } => {
                        let artifact = LazyArtifact::from(artifact.value.clone());
                        referenced_artifacts(&artifact)
                    }
                    CompleteProcessTemplateComponent::Literal { .. }
                    | CompleteProcessTemplateComponent::OutputPath
                    | CompleteProcessTemplateComponent::ResourcesDir
                    | CompleteProcessTemplateComponent::HomeDir
                    | CompleteProcessTemplateComponent::WorkDir
                    | CompleteProcessTemplateComponent::TempDir => vec![],
                })
                .chain(referenced_artifacts(&work_dir))
                .chain(output_scaffold.iter().flat_map(referenced_artifacts))
                .collect()
        }
        LazyArtifact::CreateFile {
            content: _,
            executable: _,
            resources,
        } => referenced_artifacts(resources),
        LazyArtifact::CreateDirectory(directory) => directory
            .entries
            .values()
            .flat_map(|entry| referenced_artifacts(entry))
            .collect(),
        LazyArtifact::Cast { artifact, to: _ } => referenced_artifacts(artifact),
        LazyArtifact::Merge { directories } => directories
            .iter()
            .flat_map(|dir| referenced_artifacts(dir))
            .collect(),
        LazyArtifact::Peel {
            directory,
            depth: _,
        } => referenced_artifacts(directory),
        LazyArtifact::Get { directory, path: _ } => referenced_artifacts(directory),
        LazyArtifact::Insert {
            directory,
            path: _,
            artifact,
        } => referenced_artifacts(directory)
            .into_iter()
            .chain(
                artifact
                    .iter()
                    .flat_map(|artifact| referenced_artifacts(artifact)),
            )
            .collect(),
        LazyArtifact::SetPermissions {
            file,
            executable: _,
        } => referenced_artifacts(file),
        LazyArtifact::Proxy(proxy) => vec![proxy.artifact],
    }
}

pub async fn descendent_project_resolves(
    brioche: &Brioche,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<Vec<(LazyArtifact, CompleteArtifact)>> {
    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    // Find all artifacts resolved by the project (either directly in the
    // `project_resolves` table or indirectly in the `child_resolves` table),
    // then filter it down to only `complete_process` and `download` artifacts.
    let project_hash_value = project_hash.to_string();
    let project_descendent_resolves = sqlx::query!(
        r#"
            WITH RECURSIVE project_descendent_resolves (artifact_hash) AS (
                SELECT project_resolves.artifact_hash
                FROM project_resolves
                WHERE project_hash = ? AND export = ?
                UNION
                SELECT child_resolves.artifact_hash
                FROM child_resolves
                INNER JOIN project_descendent_resolves ON
                    project_descendent_resolves.artifact_hash = child_resolves.parent_hash
            )
            SELECT
                input_artifacts.artifact_hash AS input_hash,
                input_artifacts.artifact_json AS input_json,
                output_artifacts.artifact_hash AS output_hash,
                output_artifacts.artifact_json AS output_json
            FROM project_descendent_resolves
            INNER JOIN resolves ON
                resolves.input_hash = project_descendent_resolves.artifact_hash
            INNER JOIN artifacts AS input_artifacts ON
                input_artifacts.artifact_hash = resolves.input_hash
            INNER JOIN artifacts AS output_artifacts ON
                output_artifacts.artifact_hash = resolves.output_hash
            WHERE input_artifacts.artifact_json->>'type' IN ('complete_process', 'download');
        "#,
        project_hash_value,
        export,
    )
    .fetch_all(&mut *db_transaction)
    .await?;

    let project_descendent_resolves = project_descendent_resolves
        .into_iter()
        .map(|record| {
            let input_hash = record
                .input_hash
                .parse()
                .context("invalid artifact hash from database")?;
            let output_hash = record
                .output_hash
                .parse()
                .context("invalid artifact hash from database")?;
            let input: LazyArtifact = serde_json::from_str(&record.input_json)
                .context("invalid artifact JSON from database")?;
            let output: CompleteArtifact = serde_json::from_str(&record.output_json)
                .context("invalid artifact JSON from database")?;

            anyhow::ensure!(
                input.hash() == input_hash,
                "expected input hash to be {input_hash}, but was {}",
                input.hash()
            );
            anyhow::ensure!(
                output.hash() == output_hash,
                "expected output hash to be {output_hash}, but was {}",
                output.hash()
            );

            Ok((input, output))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    db_transaction.commit().await?;

    Ok(project_descendent_resolves)
}
