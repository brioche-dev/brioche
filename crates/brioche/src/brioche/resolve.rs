use std::{collections::HashMap, sync::Arc};

use anyhow::Context as _;
use futures::{stream::FuturesUnordered, TryStreamExt as _};
use tracing::Instrument as _;

use super::{
    value::{
        CompleteValue, CompleteValueDiscriminants, Directory, File, LazyDirectory, LazyValue, Meta,
        ValueHash, WithMeta,
    },
    Brioche,
};

mod download;
mod process;
mod unpack;

#[derive(Debug, Default)]
pub struct Proxies {
    values_by_hash: HashMap<ValueHash, LazyValue>,
}

#[derive(Debug, Default)]
pub struct ActiveResolves {
    resolve_watchers:
        HashMap<ValueHash, tokio::sync::watch::Receiver<Option<Result<CompleteValue, String>>>>,
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip(brioche, value), fields(value_hash = %value.hash(), value_kind = ?value.kind(), resolved_method))]
pub async fn resolve(
    brioche: &Brioche,
    value: WithMeta<LazyValue>,
) -> anyhow::Result<WithMeta<CompleteValue>> {
    let meta = value.meta.clone();
    let value_hash = value.hash();

    // If we're currently resolving the value in another task, wait for it to
    // complete and return early
    let resolve_tx = {
        let mut active_resolves = brioche.active_resolves.write().await;
        match active_resolves.resolve_watchers.entry(value_hash) {
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
                        tracing::debug!(%value_hash, "received resolve result from in-progress resolve");
                        return Ok(WithMeta::new(resolve_result.clone(), meta));
                    }
                    Err(error) => {
                        tracing::debug!(%value_hash, %error, "received error while waiting for in-progress resolve to finish, resolve already failed");
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

    // If we have a value that can be trivially converted to a complete value,
    // avoid saving it and just return it directly
    let complete_value: Result<CompleteValue, _> = value.value.clone().try_into();
    if let Ok(complete_value) = complete_value {
        tracing::Span::current().record("resolve_method", "trivial_conversion");

        // Remove the active resolve watcher
        {
            let mut active_resolves = brioche.active_resolves.write().await;
            active_resolves.resolve_watchers.remove(&value_hash);
        }

        let _ = resolve_tx.send(Some(Ok(complete_value.clone())));

        return Ok(WithMeta::new(complete_value, meta));
    }

    // Check the database to see if we've cached this value before
    let mut db_conn = brioche.db_conn.lock().await;
    let input_hash = value_hash.to_string();
    let result = sqlx::query!(
        "SELECT output_json FROM resolves WHERE input_hash = ? LIMIT 1",
        input_hash,
    )
    .fetch_optional(&mut *db_conn)
    .await?;
    drop(db_conn);

    if let Some(row) = result {
        let complete_value: CompleteValue = serde_json::from_str(&row.output_json)?;
        tracing::Span::current().record("resolve_method", "database_hit");
        tracing::trace!(%value_hash, complete_hash = %complete_value.hash(), "got resolve result from database");

        // Remove the active resolve watcher
        {
            let mut active_resolves = brioche.active_resolves.write().await;
            active_resolves.resolve_watchers.remove(&value_hash);
        }

        let _ = resolve_tx.send(Some(Ok(complete_value.clone())));

        return Ok(WithMeta::new(complete_value, meta));
    }

    let input_json = serde_json::to_string(&value.value)?;

    // Resolve the value for real
    let result_value = tokio::spawn({
        let brioche = brioche.clone();
        let meta = meta.clone();
        async move { resolve_inner(&brioche, value.value, &meta).await }
            .instrument(tracing::debug_span!("resolve_inner_task").or_current())
    })
    .await?
    .map_err(|error| ResolveFailed {
        message: format!("{error:#}"),
        meta: meta.clone(),
    });

    // Write the resolved value to the database on success
    if let Ok(result_value) = &result_value {
        let mut db_conn = brioche.db_conn.lock().await;
        let resolve_id = ulid::Ulid::new().to_string();
        let resolve_series_id = ulid::Ulid::new().to_string();
        let input_hash = value_hash.to_string();
        let output_json = serde_json::to_string(&result_value)?;
        let output_hash = result_value.hash().to_string();
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

        tracing::trace!(%value_hash, result_hash = %output_hash, "saved resolve result to database");
    }

    // Remove the active resolve watcher
    {
        let mut active_resolves = brioche.active_resolves.write().await;
        active_resolves.resolve_watchers.remove(&value_hash);
    }

    match result_value {
        Ok(result_value) => {
            // Ignore error because channel may have closed
            let _ = resolve_tx.send(Some(Ok(result_value.clone())));
            Ok(WithMeta::new(result_value, meta))
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
    value: LazyValue,
    meta: &Arc<Meta>,
) -> anyhow::Result<CompleteValue> {
    match value {
        LazyValue::File {
            data,
            executable,
            resources,
        } => {
            let resources = LazyValue::Directory(resources);
            let resources = resolve(brioche, WithMeta::new(resources, meta.clone())).await?;
            let CompleteValue::Directory(resources) = resources.value else {
                anyhow::bail!("file resources resolved to non-directory value");
            };
            Ok(CompleteValue::File(File {
                data,
                executable,
                resources,
            }))
        }
        LazyValue::Symlink { target } => Ok(CompleteValue::Symlink { target }),
        LazyValue::Directory(LazyDirectory { entries }) => {
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
            Ok(CompleteValue::Directory(Directory { entries }))
        }
        LazyValue::Download(download) => {
            let downloaded = download::resolve_download(brioche, download).await?;
            Ok(CompleteValue::File(downloaded))
        }
        LazyValue::Unpack(unpack) => {
            let unpacked = unpack::resolve_unpack(brioche, meta, unpack).await?;
            Ok(CompleteValue::Directory(unpacked))
        }
        LazyValue::Process(process) => {
            // We call `resolve` recursively here so that two different lazy
            // processes that resolve to the same complete process will only
            // run once (since `resolve` is memoized).
            let process = process::resolve_lazy_process_to_process(brioche, process).await?;
            let result = resolve(
                brioche,
                WithMeta::new(LazyValue::CompleteProcess(process), meta.clone()),
            )
            .await?;
            Ok(result.value)
        }
        LazyValue::CompleteProcess(process) => {
            let result = process::resolve_process(brioche, meta, process).await?;
            Ok(result)
        }
        LazyValue::CreateFile {
            data,
            executable,
            resources,
        } => {
            let blob_id =
                super::blob::save_blob(brioche, &**data, super::blob::SaveBlobOptions::default())
                    .await?;

            let resources = resolve(brioche, *resources).await?;
            let CompleteValue::Directory(resources) = resources.value else {
                anyhow::bail!("file resources resolved to non-directory value");
            };

            Ok(CompleteValue::File(File {
                data: blob_id,
                executable,
                resources,
            }))
        }
        LazyValue::Cast { value, to } => {
            let result = resolve(brioche, *value).await?;
            let result_type: CompleteValueDiscriminants = (&result.value).into();
            anyhow::ensure!(result_type == to, "tried casting {result_type:?} to {to:?}");
            Ok(result.value)
        }
        LazyValue::Merge { directories } => {
            let directories = futures::future::try_join_all(
                directories.into_iter().map(|dir| resolve(brioche, dir)),
            )
            .await?;

            let merged =
                directories
                    .into_iter()
                    .try_fold(Directory::default(), |mut merged, dir| {
                        let CompleteValue::Directory(dir) = dir.value else {
                            anyhow::bail!("tried merging with non-directory value");
                        };
                        merged.merge(dir);
                        anyhow::Ok(merged)
                    })?;

            Ok(CompleteValue::Directory(merged))
        }
        LazyValue::Proxy { hash } => {
            let proxies = brioche.proxies.read().await;
            let proxy = proxies
                .values_by_hash
                .get(&hash)
                .with_context(|| {
                    format!("tried to resolve proxy value, but hash {hash:?} was not found")
                })?
                .clone();
            let resolved = resolve(brioche, WithMeta::new(proxy, meta.clone())).await?;
            Ok(resolved.value)
        }
        LazyValue::Peel { directory, depth } => {
            let mut result = resolve(brioche, *directory).await?;

            for _ in 0..depth {
                let CompleteValue::Directory(dir) = result.value else {
                    anyhow::bail!("tried peeling non-directory value");
                };
                let mut entries = dir.entries.into_iter();
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
        LazyValue::Get { directory, path } => {
            let resolved = resolve(brioche, *directory).await?;
            let CompleteValue::Directory(mut directory) = resolved.value else {
                anyhow::bail!("tried getting item from non-directory");
            };

            let Some(result) = directory.remove(&path)? else {
                anyhow::bail!("path not found in directory: {path:?}");
            };

            Ok(result.value)
        }
        LazyValue::Remove { directory, paths } => {
            let result = resolve(brioche, *directory).await?;
            let CompleteValue::Directory(mut directory) = result.value else {
                anyhow::bail!("tried removing item from non-directory");
            };

            for path in paths {
                directory.remove(&path)?;
            }

            Ok(CompleteValue::Directory(directory))
        }
        LazyValue::SetPermissions { file, executable } => {
            let result = resolve(brioche, *file).await?;
            let CompleteValue::File(mut file) = result.value else {
                anyhow::bail!("tried setting permissions on non-file");
            };

            if let Some(executable) = executable {
                file.executable = executable;
            }

            Ok(CompleteValue::File(file))
        }
    }
}

pub async fn create_proxy(brioche: &Brioche, value: LazyValue) -> LazyValue {
    if let LazyValue::Proxy { .. } = value {
        return value;
    }

    let value_hash = value.hash();
    let mut proxies = brioche.proxies.write().await;
    proxies.values_by_hash.entry(value_hash).or_insert(value);

    LazyValue::Proxy { hash: value_hash }
}

#[derive(Debug, thiserror::Error)]
struct ResolveFailed {
    message: String,
    meta: Arc<Meta>,
}

impl std::fmt::Display for ResolveFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;

        for (n, source) in self.meta.source.iter().flatten().enumerate() {
            if n == 0 {
                writeln!(f)?;
            }
            write!(f, "\n  at {source}")?;
        }

        Ok(())
    }
}
