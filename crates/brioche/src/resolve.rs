use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Context as _;
use futures::{stream::FuturesUnordered, TryStreamExt as _};
use sqlx::Acquire as _;
use tracing::Instrument as _;

use crate::{artifact::ProxyArtifact, project::ProjectHash};

use super::{
    artifact::{
        ArtifactHash, CompleteArtifact, CompleteArtifactDiscriminants, CreateDirectory, Directory,
        DirectoryListing, File, LazyArtifact, Meta, WithMeta,
    },
    Brioche,
};

mod download;
mod process;
mod unpack;

#[derive(Debug, Default)]
pub struct Proxies {
    pub artifacts_by_hash: HashMap<ArtifactHash, LazyArtifact>,
}

#[derive(Debug, Default)]
pub struct ActiveResolves {
    resolve_watchers: HashMap<
        ArtifactHash,
        tokio::sync::watch::Receiver<Option<Result<CompleteArtifact, String>>>,
    >,
}

#[derive(Debug, Clone)]
pub enum ResolveScope {
    Project {
        project_hash: ProjectHash,
        export: String,
    },
    Child {
        parent_hash: ArtifactHash,
    },
    Anonymous,
}

#[tracing::instrument(skip(brioche, artifact), fields(artifact_hash = %artifact.hash(), artifact_kind = ?artifact.kind(), resolved_method))]
pub async fn resolve(
    brioche: &Brioche,
    artifact: WithMeta<LazyArtifact>,
    scope: &ResolveScope,
) -> anyhow::Result<WithMeta<CompleteArtifact>> {
    let artifact_hash = artifact.hash();
    let result = resolve_inner(brioche, artifact).await?;

    match scope {
        ResolveScope::Project {
            project_hash,
            export,
        } => {
            let mut db_conn = brioche.db_conn.lock().await;
            let mut db_transaction = db_conn.begin().await?;

            let project_hash_value = project_hash.to_string();
            let export_value = export.to_string();
            let artifact_hash_value = artifact_hash.to_string();
            sqlx::query!(
                r#"
                    INSERT INTO project_resolves (
                        project_hash,
                        export,
                        artifact_hash
                    ) VALUES (?, ?, ?)
                    ON CONFLICT DO NOTHING
                "#,
                project_hash_value,
                export_value,
                artifact_hash_value,
            )
            .execute(&mut *db_transaction)
            .await?;

            db_transaction.commit().await?;
        }
        ResolveScope::Child { parent_hash } => {
            let mut db_conn = brioche.db_conn.lock().await;
            let mut db_transaction = db_conn.begin().await?;

            let parent_hash_value = parent_hash.to_string();
            let artifact_hash_value = artifact_hash.to_string();
            sqlx::query!(
                r#"
                    INSERT INTO child_resolves (
                        parent_hash,
                        artifact_hash
                    ) VALUES (?, ?)
                    ON CONFLICT DO NOTHING
                "#,
                parent_hash_value,
                artifact_hash_value,
            )
            .execute(&mut *db_transaction)
            .await?;

            db_transaction.commit().await?;
        }
        ResolveScope::Anonymous => {}
    }

    Ok(result)
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip(brioche, artifact), fields(artifact_hash = %artifact.hash(), artifact_kind = ?artifact.kind(), resolved_method))]
async fn resolve_inner(
    brioche: &Brioche,
    artifact: WithMeta<LazyArtifact>,
) -> anyhow::Result<WithMeta<CompleteArtifact>> {
    let meta = artifact.meta.clone();
    let artifact_hash = artifact.hash();

    // If we're currently resolving the artifact in another task, wait for it to
    // complete and return early
    let resolve_tx = {
        let mut active_resolves = brioche.active_resolves.write().await;
        match active_resolves.resolve_watchers.entry(artifact_hash) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let mut active_resolve = entry.get().clone();

                // Make sure we don't hold the lock while waiting for the resolve to finish
                drop(active_resolves);

                let resolve_result = active_resolve
                    .wait_for(Option::is_some)
                    .await
                    .context("expected resolve result")?;

                tracing::Span::current().record("resolve_method", "active_resolve");

                let resolve_result = resolve_result.as_ref().expect("expected resolve result");
                match resolve_result {
                    Ok(resolve_result) => {
                        tracing::debug!(%artifact_hash, "received resolve result from in-progress resolve");
                        return Ok(WithMeta::new(resolve_result.clone(), meta));
                    }
                    Err(error) => {
                        tracing::debug!(%artifact_hash, %error, "received error while waiting for in-progress resolve to finish, resolve already failed");
                        anyhow::bail!("{error}");
                    }
                };
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let (resolve_tx, resolve_rx) = tokio::sync::watch::channel(None);
                entry.insert(resolve_rx);
                resolve_tx
            }
        }
    };

    // If we have a artifact that can be trivially converted to a complete
    // artifact, avoid saving it and just return it directly
    let complete_artifact: Result<CompleteArtifact, _> = artifact.value.clone().try_into();
    if let Ok(complete_artifact) = complete_artifact {
        tracing::Span::current().record("resolve_method", "trivial_conversion");

        // Remove the active resolve watcher
        {
            let mut active_resolves = brioche.active_resolves.write().await;
            active_resolves.resolve_watchers.remove(&artifact_hash);
        }

        let _ = resolve_tx.send(Some(Ok(complete_artifact.clone())));

        return Ok(WithMeta::new(complete_artifact, meta));
    }

    // Check the database to see if we've cached this artifact before
    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;
    let input_hash = artifact_hash.to_string();
    let result = sqlx::query!(
        r#"
            SELECT output_artifacts.artifact_json
            FROM resolves
            INNER JOIN artifacts AS output_artifacts
                ON resolves.output_hash = output_artifacts.artifact_hash
            WHERE resolves.input_hash = ?
            LIMIT 1
        "#,
        input_hash,
    )
    .fetch_optional(&mut *db_transaction)
    .await?;
    db_transaction.commit().await?;
    drop(db_conn);

    if let Some(row) = result {
        let complete_artifact: CompleteArtifact = serde_json::from_str(&row.artifact_json)?;
        tracing::Span::current().record("resolve_method", "database_hit");
        tracing::trace!(%artifact_hash, complete_hash = %complete_artifact.hash(), "got resolve result from database");

        // Remove the active resolve watcher
        {
            let mut active_resolves = brioche.active_resolves.write().await;
            active_resolves.resolve_watchers.remove(&artifact_hash);
        }

        let _ = resolve_tx.send(Some(Ok(complete_artifact.clone())));

        return Ok(WithMeta::new(complete_artifact, meta));
    }

    let input_json = serde_json::to_string(&artifact.value)?;

    // Try to get the artifact from the registry (if it might be expensive
    // to resolve)
    let registry_response = if artifact.is_expensive_to_resolve() {
        brioche
            .registry_client
            .get_resolve(artifact_hash)
            .await
            .ok()
    } else {
        None
    };

    // Resolve the artifact for real if we didn't get it from the registry
    let result_artifact = match registry_response {
        Some(response) => Ok(response.output_artifact),
        None => {
            let resolve_fut = {
                let brioche = brioche.clone();
                let meta = meta.clone();
                async move { run_resolve(&brioche, artifact.value, &meta).await }
                    .instrument(tracing::debug_span!("run_resolve_task").or_current())
            };
            tokio::spawn(resolve_fut)
                .await?
                .map_err(|error| ResolveFailed {
                    message: format!("{error:#}"),
                    meta: meta.clone(),
                })
        }
    };

    // Write the resolved artifact to the database on success
    if let Ok(result_artifact) = &result_artifact {
        let mut db_conn = brioche.db_conn.lock().await;
        let mut db_transaction = db_conn.begin().await?;
        let input_hash = artifact_hash.to_string();
        let output_json = serde_json::to_string(&result_artifact)?;
        let output_hash = result_artifact.hash().to_string();
        sqlx::query!(
            r#"
                INSERT INTO artifacts (artifact_hash, artifact_json)
                VALUES
                    (?, ?),
                    (?, ?)
                ON CONFLICT (artifact_hash) DO NOTHING
            "#,
            input_hash,
            input_json,
            output_hash,
            output_json,
        )
        .execute(&mut *db_transaction)
        .await?;
        sqlx::query!(
            "INSERT INTO resolves (input_hash, output_hash) VALUES (?, ?)",
            input_hash,
            output_hash,
        )
        .execute(&mut *db_transaction)
        .await?;
        db_transaction.commit().await?;

        tracing::trace!(%artifact_hash, result_hash = %output_hash, "saved resolve result to database");
    }

    // Remove the active resolve watcher
    {
        let mut active_resolves = brioche.active_resolves.write().await;
        active_resolves.resolve_watchers.remove(&artifact_hash);
    }

    match result_artifact {
        Ok(result_artifact) => {
            // Ignore error because channel may have closed
            let _ = resolve_tx.send(Some(Ok(result_artifact.clone())));
            Ok(WithMeta::new(result_artifact, meta))
        }
        Err(error) => {
            // Ignore error because channel may have closed
            let _ = resolve_tx.send(Some(Err(format!("{error:#}"))));
            Err(error.into())
        }
    }
}

#[tracing::instrument(skip_all, err)]
async fn run_resolve(
    brioche: &Brioche,
    artifact: LazyArtifact,
    meta: &Arc<Meta>,
) -> anyhow::Result<CompleteArtifact> {
    let scope = ResolveScope::Child {
        parent_hash: artifact.hash(),
    };

    match artifact {
        LazyArtifact::File {
            content_blob,
            executable,
            resources,
        } => {
            let resources = resolve(brioche, *resources, &scope).await?;
            let CompleteArtifact::Directory(resources) = resources.value else {
                anyhow::bail!("file resources resolved to non-directory value");
            };
            Ok(CompleteArtifact::File(File {
                content_blob,
                executable,
                resources,
            }))
        }
        LazyArtifact::Directory(directory) => Ok(CompleteArtifact::Directory(directory)),
        LazyArtifact::Symlink { target } => Ok(CompleteArtifact::Symlink { target }),
        LazyArtifact::Download(download) => {
            let downloaded = download::resolve_download(brioche, download).await?;
            Ok(CompleteArtifact::File(downloaded))
        }
        LazyArtifact::Unpack(unpack) => {
            let unpacked = unpack::resolve_unpack(brioche, meta, unpack).await?;
            Ok(CompleteArtifact::Directory(unpacked))
        }
        LazyArtifact::Process(process) => {
            // We call `resolve` recursively here so that two different
            // lazy processes that resolve to the same complete process will
            // only run once (since `resolve` is memoized).
            let process =
                process::resolve_lazy_process_to_process(brioche, &scope, process).await?;
            let result = resolve(
                brioche,
                WithMeta::new(LazyArtifact::CompleteProcess(process), meta.clone()),
                &scope,
            )
            .await?;
            Ok(result.value)
        }
        LazyArtifact::CompleteProcess(process) => {
            let result = process::resolve_process(brioche, meta, process).await?;
            Ok(result)
        }
        LazyArtifact::CreateFile {
            content,
            executable,
            resources,
        } => {
            let blob_hash =
                super::blob::save_blob(brioche, &content, super::blob::SaveBlobOptions::default())
                    .await?;

            let resources = resolve(brioche, *resources, &scope).await?;
            let CompleteArtifact::Directory(resources) = resources.value else {
                anyhow::bail!("file resources resolved to non-directory value");
            };

            Ok(CompleteArtifact::File(File {
                content_blob: blob_hash,
                executable,
                resources,
            }))
        }
        LazyArtifact::CreateDirectory(CreateDirectory { entries }) => {
            let entries = entries
                .into_iter()
                .map(|(path, entry)| {
                    let brioche = brioche.clone();
                    let scope = scope.clone();
                    async move {
                        let entry = resolve(&brioche, entry, &scope).await?;
                        anyhow::Ok((path, entry))
                    }
                })
                .collect::<FuturesUnordered<_>>()
                .try_collect()
                .await?;
            let listing = DirectoryListing { entries };
            let directory = Directory::create(brioche, &listing).await?;
            Ok(CompleteArtifact::Directory(directory))
        }
        LazyArtifact::Cast { artifact, to } => {
            let result = resolve(brioche, *artifact, &scope).await?;
            let result_type: CompleteArtifactDiscriminants = (&result.value).into();
            anyhow::ensure!(result_type == to, "tried casting {result_type:?} to {to:?}");
            Ok(result.value)
        }
        LazyArtifact::Merge { directories } => {
            let directories = futures::future::try_join_all(
                directories
                    .into_iter()
                    .map(|dir| resolve(brioche, dir, &scope)),
            )
            .await?;

            let mut merged = DirectoryListing::default();
            for dir in directories {
                let CompleteArtifact::Directory(dir) = dir.value else {
                    anyhow::bail!("tried merging non-directory artifact");
                };
                let listing = dir.listing(brioche).await?;
                merged.merge(&listing, brioche).await?;
            }

            let merged = Directory::create(brioche, &merged).await?;
            Ok(CompleteArtifact::Directory(merged))
        }
        LazyArtifact::Proxy(proxy) => {
            let inner = proxy.inner(brioche).await?;
            let resolved = resolve(brioche, WithMeta::new(inner, meta.clone()), &scope).await?;
            Ok(resolved.value)
        }
        LazyArtifact::Peel { directory, depth } => {
            let mut result = resolve(brioche, *directory, &scope).await?;

            for _ in 0..depth {
                let CompleteArtifact::Directory(dir) = result.value else {
                    anyhow::bail!("tried peeling non-directory artifact");
                };
                let listing = dir.listing(brioche).await?;
                let mut entries = listing.entries.into_iter();
                let Some((_, peeled)) = entries.next() else {
                    anyhow::bail!("tried peeling empty directory");
                };

                if entries.next().is_some() {
                    anyhow::bail!("tried peeling directory with multiple entries");
                }

                result = peeled;
            }

            Ok(result.value)
        }
        LazyArtifact::Get { directory, path } => {
            let resolved = resolve(brioche, *directory, &scope).await?;
            let CompleteArtifact::Directory(directory) = resolved.value else {
                anyhow::bail!("tried getting item from non-directory");
            };
            let listing = directory.listing(brioche).await?;

            let Some(result) = listing.get(brioche, &path).await? else {
                anyhow::bail!("path not found in directory: {path:?}");
            };

            Ok(result.value)
        }
        LazyArtifact::Insert {
            directory,
            path,
            artifact,
        } => {
            let (directory, artifact) = tokio::try_join!(resolve(brioche, *directory, &scope), {
                let scope = scope.clone();
                async move {
                    match artifact {
                        Some(artifact) => Ok(Some(resolve(brioche, *artifact, &scope).await?)),
                        None => Ok(None),
                    }
                }
            })?;

            let CompleteArtifact::Directory(directory) = directory.value else {
                anyhow::bail!("tried removing item from non-directory artifact");
            };
            let mut listing = directory.listing(brioche).await?;

            listing.insert(brioche, &path, artifact).await?;

            let new_directory = Directory::create(brioche, &listing).await?;
            Ok(CompleteArtifact::Directory(new_directory))
        }
        LazyArtifact::SetPermissions { file, executable } => {
            let result = resolve(brioche, *file, &scope).await?;
            let CompleteArtifact::File(mut file) = result.value else {
                anyhow::bail!("tried setting permissions on non-file");
            };

            if let Some(executable) = executable {
                file.executable = executable;
            }

            Ok(CompleteArtifact::File(file))
        }
    }
}

pub async fn create_proxy(
    brioche: &Brioche,
    artifact: LazyArtifact,
) -> anyhow::Result<LazyArtifact> {
    if let LazyArtifact::Proxy { .. } = artifact {
        return Ok(artifact);
    }

    let artifact_hash = artifact.hash();
    crate::artifact::save_artifacts(brioche, [artifact]).await?;

    Ok(LazyArtifact::Proxy(ProxyArtifact {
        artifact: artifact_hash,
    }))
}

#[derive(Debug, thiserror::Error)]
struct ResolveFailed {
    message: String,
    meta: Arc<Meta>,
}

impl std::fmt::Display for ResolveFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (message_sources, mut message_lines) = self
            .message
            .lines()
            .partition::<Vec<_>, _>(|line| line.trim_start().starts_with("at "));
        message_lines.retain(|line| !line.trim().is_empty());
        let message = message_lines.join("\n");

        let mut sources = vec![];
        let mut seen_sources = HashSet::new();

        // HACK: Currently, detailed errors get converted to and from strings
        // via `anyhow`. To properly print all source lines without duplicates,
        // we do our best to parse the error message. This should instead be
        // handled by keeping structured errors throughout.
        for source in message_sources {
            let source = source
                .trim_start()
                .strip_prefix("at ")
                .expect("invalid line")
                .to_string();
            if seen_sources.insert(source.clone()) {
                sources.push(source);
            }
        }

        for source in self.meta.source.iter().flatten() {
            let source = source.to_string();
            if seen_sources.insert(source.clone()) {
                sources.push(source);
            }
        }

        write!(f, "{message}")?;
        for source in &sources {
            write!(f, "\n    at {source}")?;
        }

        Ok(())
    }
}
