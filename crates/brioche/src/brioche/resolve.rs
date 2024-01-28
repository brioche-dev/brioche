use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Context as _;
use futures::{stream::FuturesUnordered, TryStreamExt as _};
use tracing::Instrument as _;

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
    artifacts_by_hash: HashMap<ArtifactHash, LazyArtifact>,
}

#[derive(Debug, Default)]
pub struct ActiveResolves {
    resolve_watchers: HashMap<
        ArtifactHash,
        tokio::sync::watch::Receiver<Option<Result<CompleteArtifact, String>>>,
    >,
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip(brioche, artifact), fields(artifact_hash = %artifact.hash(), artifact_kind = ?artifact.kind(), resolved_method))]
pub async fn resolve(
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
    let input_hash = artifact_hash.to_string();
    let result = sqlx::query!(
        "SELECT output_json FROM resolves WHERE input_hash = ? LIMIT 1",
        input_hash,
    )
    .fetch_optional(&mut *db_conn)
    .await?;
    drop(db_conn);

    if let Some(row) = result {
        let complete_artifact: CompleteArtifact = serde_json::from_str(&row.output_json)?;
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

    // Resolve the artifact for real
    let result_artifact = tokio::spawn({
        let brioche = brioche.clone();
        let meta = meta.clone();
        async move { resolve_inner(&brioche, artifact.value, &meta).await }
            .instrument(tracing::debug_span!("resolve_inner_task").or_current())
    })
    .await?
    .map_err(|error| ResolveFailed {
        message: format!("{error:#}"),
        meta: meta.clone(),
    });

    // Write the resolved artifact to the database on success
    if let Ok(result_artifact) = &result_artifact {
        let mut db_conn = brioche.db_conn.lock().await;
        let resolve_id = ulid::Ulid::new().to_string();
        let resolve_series_id = ulid::Ulid::new().to_string();
        let input_hash = artifact_hash.to_string();
        let output_json = serde_json::to_string(&result_artifact)?;
        let output_hash = result_artifact.hash().to_string();
        sqlx::query!(
            "INSERT INTO resolves (id, series_id, input_json, input_hash, output_json, output_hash) VALUES (?, ?, ?, ?, ?, ?)",
            resolve_id,
            resolve_series_id,
            input_json,
            input_hash,
            output_json,
            output_hash,
        )
        .execute(&mut *db_conn)
        .await?;

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
async fn resolve_inner(
    brioche: &Brioche,
    artifact: LazyArtifact,
    meta: &Arc<Meta>,
) -> anyhow::Result<CompleteArtifact> {
    match artifact {
        LazyArtifact::File {
            content_blob,
            executable,
            resources,
        } => {
            let resources = resolve(brioche, *resources).await?;
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
            // We call `resolve` recursively here so that two different lazy
            // processes that resolve to the same complete process will only
            // run once (since `resolve` is memoized).
            let process = process::resolve_lazy_process_to_process(brioche, process).await?;
            let result = resolve(
                brioche,
                WithMeta::new(LazyArtifact::CompleteProcess(process), meta.clone()),
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
            let blob_id =
                super::blob::save_blob(brioche, &content, super::blob::SaveBlobOptions::default())
                    .await?;

            let resources = resolve(brioche, *resources).await?;
            let CompleteArtifact::Directory(resources) = resources.value else {
                anyhow::bail!("file resources resolved to non-directory value");
            };

            Ok(CompleteArtifact::File(File {
                content_blob: blob_id,
                executable,
                resources,
            }))
        }
        LazyArtifact::CreateDirectory(CreateDirectory { entries }) => {
            let entries = entries
                .into_iter()
                .map(|(path, entry)| {
                    let brioche = brioche.clone();
                    async move {
                        let entry = resolve(&brioche, entry).await?;
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
            let result = resolve(brioche, *artifact).await?;
            let result_type: CompleteArtifactDiscriminants = (&result.value).into();
            anyhow::ensure!(result_type == to, "tried casting {result_type:?} to {to:?}");
            Ok(result.value)
        }
        LazyArtifact::Merge { directories } => {
            let directories = futures::future::try_join_all(
                directories.into_iter().map(|dir| resolve(brioche, dir)),
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
        LazyArtifact::Proxy { hash } => {
            let proxy = {
                let proxies = brioche.proxies.read().await;
                proxies
                    .artifacts_by_hash
                    .get(&hash)
                    .with_context(|| {
                        format!("tried to resolve proxy artifact, but hash {hash:?} was not found")
                    })?
                    .clone()
            };
            let resolved = resolve(brioche, WithMeta::new(proxy, meta.clone())).await?;
            Ok(resolved.value)
        }
        LazyArtifact::Peel { directory, depth } => {
            let mut result = resolve(brioche, *directory).await?;

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
            let resolved = resolve(brioche, *directory).await?;
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
            let (directory, artifact) =
                tokio::try_join!(resolve(brioche, *directory), async move {
                    match artifact {
                        Some(artifact) => Ok(Some(resolve(brioche, *artifact).await?)),
                        None => Ok(None),
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
            let result = resolve(brioche, *file).await?;
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

pub async fn create_proxy(brioche: &Brioche, artifact: LazyArtifact) -> LazyArtifact {
    if let LazyArtifact::Proxy { .. } = artifact {
        return artifact;
    }

    let artifact_hash = artifact.hash();
    let mut proxies = brioche.proxies.write().await;
    proxies
        .artifacts_by_hash
        .entry(artifact_hash)
        .or_insert(artifact);

    LazyArtifact::Proxy {
        hash: artifact_hash,
    }
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
