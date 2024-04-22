use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Context as _;
use sqlx::Acquire as _;

use crate::{
    artifact::{
        ArtifactHash, CompleteArtifact, CompleteProcessArtifact, LazyArtifact, ProcessArtifact,
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
    artifact: LazyArtifact,
) -> anyhow::Result<()> {
    let mut unvisited = VecDeque::from_iter([artifact]);
    while let Some(artifact) = unvisited.pop_front() {
        let hash = artifact.hash();
        match references.artifacts.entry(hash) {
            std::collections::hash_map::Entry::Occupied(_) => {
                // Already visited
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(artifact.clone());

                match artifact {
                    LazyArtifact::File {
                        content_blob,
                        executable: _,
                        resources,
                    } => {
                        references.blobs.insert(content_blob);
                        unvisited.push_back(resources.value);
                    }
                    LazyArtifact::Directory(directory) => {
                        if let Some(listing_blob) = directory.listing_blob {
                            references.blobs.insert(listing_blob);
                        }

                        let listing = directory.listing(brioche).await?;
                        for (_, artifact) in listing.entries {
                            unvisited.push_back(LazyArtifact::from(artifact.value));
                        }
                    }
                    LazyArtifact::Symlink { .. } => {}
                    LazyArtifact::Download(_) => {}
                    LazyArtifact::Unpack(unpack) => {
                        unvisited.push_back(unpack.file.value);
                    }
                    LazyArtifact::Process(process) => {
                        let ProcessArtifact {
                            command,
                            args,
                            env,
                            work_dir,
                            output_scaffold,
                            platform: _,
                        } = process;

                        let templates = [command].into_iter().chain(args).chain(env.into_values());

                        for template in templates {
                            for component in template.components {
                                match component {
                                    crate::artifact::ProcessTemplateComponent::Input {
                                        artifact,
                                    } => {
                                        unvisited.push_back(artifact.value);
                                    }
                                    crate::artifact::ProcessTemplateComponent::Literal {
                                        ..
                                    }
                                    | crate::artifact::ProcessTemplateComponent::OutputPath
                                    | crate::artifact::ProcessTemplateComponent::ResourcesDir
                                    | crate::artifact::ProcessTemplateComponent::HomeDir
                                    | crate::artifact::ProcessTemplateComponent::WorkDir
                                    | crate::artifact::ProcessTemplateComponent::TempDir => {}
                                }
                            }
                        }

                        unvisited.push_back(work_dir.value);
                        if let Some(output_scaffold) = output_scaffold {
                            unvisited.push_back(output_scaffold.value);
                        }
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

                        let templates = [command].into_iter().chain(args).chain(env.into_values());

                        for template in templates {
                            for component in template.components {
                                match component {
                                    crate::artifact::CompleteProcessTemplateComponent::Input {
                                        artifact,
                                    } => {
                                        unvisited.push_back(LazyArtifact::from(artifact.value));
                                    }
                                    crate::artifact::CompleteProcessTemplateComponent::Literal {
                                        ..
                                    }
                                    | crate::artifact::CompleteProcessTemplateComponent::OutputPath
                                    | crate::artifact::CompleteProcessTemplateComponent::ResourcesDir
                                    | crate::artifact::CompleteProcessTemplateComponent::HomeDir
                                    | crate::artifact::CompleteProcessTemplateComponent::WorkDir
                                    | crate::artifact::CompleteProcessTemplateComponent::TempDir => {}
                                }
                            }
                        }

                        unvisited.push_back(LazyArtifact::from(work_dir));
                        if let Some(output_scaffold) = output_scaffold {
                            unvisited.push_back(LazyArtifact::from(*output_scaffold));
                        }
                    }
                    LazyArtifact::CreateFile {
                        content: _,
                        executable: _,
                        resources,
                    } => {
                        unvisited.push_back(resources.value);
                    }
                    LazyArtifact::CreateDirectory(directory) => {
                        for (_, artifact) in directory.entries {
                            unvisited.push_back(artifact.value);
                        }
                    }
                    LazyArtifact::Cast { artifact, to: _ } => {
                        unvisited.push_back(artifact.value);
                    }
                    LazyArtifact::Merge { directories } => {
                        for directory in directories {
                            unvisited.push_back(directory.value);
                        }
                    }
                    LazyArtifact::Peel {
                        directory,
                        depth: _,
                    } => {
                        unvisited.push_back(directory.value);
                    }
                    LazyArtifact::Get { directory, path: _ } => {
                        unvisited.push_back(directory.value);
                    }
                    LazyArtifact::Insert {
                        directory,
                        path: _,
                        artifact,
                    } => {
                        unvisited.push_back(directory.value);
                        if let Some(artifact) = artifact {
                            unvisited.push_back(artifact.value);
                        }
                    }
                    LazyArtifact::SetPermissions {
                        file,
                        executable: _,
                    } => {
                        unvisited.push_back(file.value);
                    }
                    LazyArtifact::Proxy(proxy) => {
                        let inner = proxy.inner(brioche).await?;
                        unvisited.push_back(inner);
                    }
                }
            }
        }
    }

    Ok(())
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
