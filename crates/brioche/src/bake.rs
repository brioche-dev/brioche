use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Context as _;
use futures::{stream::FuturesUnordered, TryStreamExt as _};
use sqlx::Acquire as _;
use tracing::Instrument as _;

use crate::{
    project::ProjectHash,
    recipe::{ArtifactDiscriminants, ProxyRecipe},
};

use super::{
    recipe::{Artifact, CreateDirectory, Directory, File, Meta, Recipe, RecipeHash, WithMeta},
    Brioche,
};

mod download;
mod process;
mod unarchive;

#[derive(Debug, Default)]
pub struct CachedRecipes {
    pub recipes_by_hash: HashMap<RecipeHash, Recipe>,
}

#[derive(Debug, Default)]
pub struct ActiveBakes {
    bake_watchers:
        HashMap<RecipeHash, tokio::sync::watch::Receiver<Option<Result<Artifact, String>>>>,
}

#[derive(Debug, Clone)]
pub enum BakeScope {
    Project {
        project_hash: ProjectHash,
        export: String,
    },
    Child {
        parent_hash: RecipeHash,
    },
    Anonymous,
}

#[tracing::instrument(skip(brioche, recipe), fields(recipe_hash = %recipe.hash(), recipe_kind = ?recipe.kind(), bake_method))]
pub async fn bake(
    brioche: &Brioche,
    recipe: WithMeta<Recipe>,
    scope: &BakeScope,
) -> anyhow::Result<WithMeta<Artifact>> {
    let recipe_hash = recipe.hash();
    let result = bake_inner(brioche, recipe).await?;

    match scope {
        BakeScope::Project {
            project_hash,
            export,
        } => {
            let mut db_conn = brioche.db_conn.lock().await;
            let mut db_transaction = db_conn.begin().await?;

            let project_hash_value = project_hash.to_string();
            let export_value = export.to_string();
            let recipe_hash_value = recipe_hash.to_string();
            sqlx::query!(
                r#"
                    INSERT INTO project_bakes (
                        project_hash,
                        export,
                        recipe_hash
                    ) VALUES (?, ?, ?)
                    ON CONFLICT (project_hash, export, recipe_hash) DO NOTHING
                "#,
                project_hash_value,
                export_value,
                recipe_hash_value,
            )
            .execute(&mut *db_transaction)
            .await?;

            db_transaction.commit().await?;
        }
        BakeScope::Child { parent_hash } => {
            let mut db_conn = brioche.db_conn.lock().await;
            let mut db_transaction = db_conn.begin().await?;

            let parent_hash_value = parent_hash.to_string();
            let recipe_hash_value = recipe_hash.to_string();
            sqlx::query!(
                r#"
                    INSERT INTO child_bakes (
                        parent_hash,
                        recipe_hash
                    ) VALUES (?, ?)
                    ON CONFLICT (parent_hash, recipe_hash) DO NOTHING
                "#,
                parent_hash_value,
                recipe_hash_value,
            )
            .execute(&mut *db_transaction)
            .await?;

            db_transaction.commit().await?;
        }
        BakeScope::Anonymous => {}
    }

    Ok(result)
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip(brioche, recipe), fields(recipe_hash = %recipe.hash(), recipe_kind = ?recipe.kind(), bake_method))]
async fn bake_inner(
    brioche: &Brioche,
    recipe: WithMeta<Recipe>,
) -> anyhow::Result<WithMeta<Artifact>> {
    let meta = recipe.meta.clone();
    let recipe_hash = recipe.hash();

    // If we're currently resolving the recipe in another task, wait for it to
    // complete and return early
    let bake_tx = {
        let mut active_bakes = brioche.active_bakes.write().await;
        match active_bakes.bake_watchers.entry(recipe_hash) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let mut active_bake = entry.get().clone();

                // Make sure we don't hold the lock while waiting for the bake to finish
                drop(active_bakes);

                let bake_result = active_bake
                    .wait_for(Option::is_some)
                    .await
                    .context("expected bake result")?;

                tracing::Span::current().record("bake_method", "active_bake");

                let bake_result = bake_result.as_ref().expect("expected bake result");
                match bake_result {
                    Ok(bake_result) => {
                        tracing::debug!(%recipe_hash, "received bake result from in-progress bake");
                        return Ok(WithMeta::new(bake_result.clone(), meta));
                    }
                    Err(error) => {
                        tracing::debug!(%recipe_hash, %error, "received error while waiting for in-progress bake to finish, bake already failed");
                        anyhow::bail!("{error}");
                    }
                };
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let (bake_tx, bake_rx) = tokio::sync::watch::channel(None);
                entry.insert(bake_rx);
                bake_tx
            }
        }
    };

    // If we have a recipe that can be trivially converted to an artifact,
    // avoid saving it and just return it directly
    let artifact: Result<Artifact, _> = recipe.value.clone().try_into();
    if let Ok(artifact) = artifact {
        tracing::Span::current().record("bake_method", "trivial_conversion");

        // Remove the active bake watcher
        {
            let mut active_bakes = brioche.active_bakes.write().await;
            active_bakes.bake_watchers.remove(&recipe_hash);
        }

        let _ = bake_tx.send(Some(Ok(artifact.clone())));

        return Ok(WithMeta::new(artifact, meta));
    }

    // Check the database to see if we've cached this recipe before
    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;
    let input_hash = recipe_hash.to_string();
    let result = sqlx::query!(
        r#"
            SELECT output_artifacts.recipe_json AS artifact_json
            FROM bakes
            INNER JOIN recipes AS output_artifacts
                ON bakes.output_hash = output_artifacts.recipe_hash
            WHERE bakes.input_hash = ?
            LIMIT 1
        "#,
        input_hash,
    )
    .fetch_optional(&mut *db_transaction)
    .await?;
    db_transaction.commit().await?;
    drop(db_conn);

    if let Some(row) = result {
        let artifact: Artifact = serde_json::from_str(&row.artifact_json)?;
        tracing::Span::current().record("bake_method", "database_hit");
        tracing::trace!(%recipe_hash, artifact_hash = %artifact.hash(), "got bake result from database");

        // Remove the active bake watcher
        {
            let mut active_bakes = brioche.active_bakes.write().await;
            active_bakes.bake_watchers.remove(&recipe_hash);
        }

        let _ = bake_tx.send(Some(Ok(artifact.clone())));

        return Ok(WithMeta::new(artifact, meta));
    }

    let input_json = serde_json::to_string(&recipe.value)?;

    // Try to get the baked recipe from the registry (if it might be
    // expensive to bake)
    let registry_response = if recipe.is_expensive_to_bake() {
        brioche.registry_client.get_bake(recipe_hash).await.ok()
    } else {
        None
    };

    let result_artifact = match registry_response {
        Some(response) => {
            // The registry has the baked recipe, so fetch the references
            // and return the output artifact
            crate::registry::fetch_bake_references(brioche.clone(), response.clone()).await?;
            Ok(response.output_artifact)
        }
        None => {
            // Bake the recipe for real if we didn't get it from the registry
            let bake_fut = {
                let brioche = brioche.clone();
                let meta = meta.clone();
                async move {
                    // Clone the recipe (but only if we are going to sync it)
                    let input_recipe = if recipe.is_expensive_to_bake() {
                        Some(recipe.value.clone())
                    } else {
                        None
                    };

                    // Bake the recipe
                    let baked = run_bake(&brioche, recipe.value, &meta).await?;

                    // Send expensive recipes to optionally be synced to
                    // the registry right afer we baked it
                    if let Some(input_recipe) = input_recipe {
                        brioche
                            .sync_tx
                            .send(crate::SyncMessage::StartSync {
                                brioche: brioche.clone(),
                                recipe: input_recipe,
                                artifact: baked.clone(),
                            })
                            .await?;
                    }

                    anyhow::Ok(baked)
                }
                .instrument(tracing::debug_span!("run_bake_task").or_current())
            };
            tokio::spawn(bake_fut).await?.map_err(|error| BakeFailed {
                message: format!("{error:#}"),
                meta: meta.clone(),
            })
        }
    };

    // Write the baked recipe to the database on success
    if let Ok(artifact) = &result_artifact {
        let mut db_conn = brioche.db_conn.lock().await;
        let mut db_transaction = db_conn.begin().await?;
        let input_hash = recipe_hash.to_string();
        let output_json = serde_json::to_string(&artifact)?;
        let output_hash = artifact.hash().to_string();
        sqlx::query!(
            r#"
                INSERT INTO recipes (recipe_hash, recipe_json)
                VALUES
                    (?, ?),
                    (?, ?)
                ON CONFLICT (recipe_hash) DO NOTHING
            "#,
            input_hash,
            input_json,
            output_hash,
            output_json,
        )
        .execute(&mut *db_transaction)
        .await?;
        sqlx::query!(
            r#"
                INSERT INTO bakes (input_hash, output_hash)
                VALUES (?, ?)
                ON CONFLICT (input_hash, output_hash) DO NOTHING
            "#,
            input_hash,
            output_hash,
        )
        .execute(&mut *db_transaction)
        .await?;
        db_transaction.commit().await?;

        tracing::trace!(%recipe_hash, result_hash = %output_hash, "saved bake result to database");
    }

    // Remove the active bake watcher
    {
        let mut active_bakes = brioche.active_bakes.write().await;
        active_bakes.bake_watchers.remove(&recipe_hash);
    }

    match result_artifact {
        Ok(result_artifact) => {
            // Ignore error because channel may have closed
            let _ = bake_tx.send(Some(Ok(result_artifact.clone())));
            Ok(WithMeta::new(result_artifact, meta))
        }
        Err(error) => {
            // Ignore error because channel may have closed
            let _ = bake_tx.send(Some(Err(format!("{error:#}"))));
            Err(error.into())
        }
    }
}

#[tracing::instrument(skip_all, err)]
async fn run_bake(brioche: &Brioche, recipe: Recipe, meta: &Arc<Meta>) -> anyhow::Result<Artifact> {
    let scope = BakeScope::Child {
        parent_hash: recipe.hash(),
    };

    match recipe {
        Recipe::File {
            content_blob,
            executable,
            resources,
        } => {
            let resources = bake(brioche, *resources, &scope).await?;
            let Artifact::Directory(resources) = resources.value else {
                anyhow::bail!("file resources recipe returned non-directory artifact");
            };
            Ok(Artifact::File(File {
                content_blob,
                executable,
                resources,
            }))
        }
        Recipe::Directory(directory) => Ok(Artifact::Directory(directory)),
        Recipe::Symlink { target } => Ok(Artifact::Symlink { target }),
        Recipe::Download(download) => {
            let downloaded = download::bake_download(brioche, download).await?;
            Ok(Artifact::File(downloaded))
        }
        Recipe::Unarchive(unarchive) => {
            let unarchived = unarchive::bake_unarchive(brioche, &scope, meta, unarchive).await?;
            Ok(Artifact::Directory(unarchived))
        }
        Recipe::Process(process) => {
            // We call `bake` recursively here so that two different
            // lazy processes that bake to the same complete process will
            // only run once (since `bake` is memoized).
            let process = process::bake_lazy_process_to_process(brioche, &scope, process).await?;
            let result = bake(
                brioche,
                WithMeta::new(Recipe::CompleteProcess(process), meta.clone()),
                &scope,
            )
            .await?;
            Ok(result.value)
        }
        Recipe::CompleteProcess(process) => {
            let result = process::bake_process(brioche, meta, process).await?;
            Ok(result)
        }
        Recipe::CreateFile {
            content,
            executable,
            resources,
        } => {
            let blob_hash =
                super::blob::save_blob(brioche, &content, super::blob::SaveBlobOptions::default())
                    .await?;

            let resources = bake(brioche, *resources, &scope).await?;
            let Artifact::Directory(resources) = resources.value else {
                anyhow::bail!("file resources recipe returned non-directory artifact");
            };

            Ok(Artifact::File(File {
                content_blob: blob_hash,
                executable,
                resources,
            }))
        }
        Recipe::CreateDirectory(CreateDirectory { entries }) => {
            let entries = entries
                .into_iter()
                .map(|(path, entry)| {
                    let brioche = brioche.clone();
                    let scope = scope.clone();
                    async move {
                        let entry = bake(&brioche, entry, &scope).await?;
                        anyhow::Ok((path, entry))
                    }
                })
                .collect::<FuturesUnordered<_>>()
                .try_collect()
                .await?;
            let directory = Directory::create(brioche, &entries).await?;
            Ok(Artifact::Directory(directory))
        }
        Recipe::Cast { recipe, to } => {
            let result = bake(brioche, *recipe, &scope).await?;
            let result_type: ArtifactDiscriminants = (&result.value).into();
            anyhow::ensure!(result_type == to, "tried casting {result_type:?} to {to:?}");
            Ok(result.value)
        }
        Recipe::Merge { directories } => {
            let directories = futures::future::try_join_all(
                directories
                    .into_iter()
                    .map(|dir| bake(brioche, dir, &scope)),
            )
            .await?;

            let mut merged = Directory::default();
            for dir in directories {
                let Artifact::Directory(dir) = dir.value else {
                    anyhow::bail!("tried merging non-directory artifact");
                };
                merged.merge(&dir, brioche).await?;
            }

            Ok(Artifact::Directory(merged))
        }
        Recipe::Proxy(proxy) => {
            let inner = proxy.inner(brioche).await?;
            let artifact = bake(brioche, WithMeta::new(inner, meta.clone()), &scope).await?;
            Ok(artifact.value)
        }
        Recipe::Peel { directory, depth } => {
            let mut result = bake(brioche, *directory, &scope).await?;

            for _ in 0..depth {
                let Artifact::Directory(dir) = result.value else {
                    anyhow::bail!("tried peeling non-directory artifact");
                };
                let entries = dir.entries(brioche).await?;
                let mut entries = entries.into_values();
                let Some(peeled) = entries.next() else {
                    anyhow::bail!("tried peeling empty directory");
                };

                if entries.next().is_some() {
                    anyhow::bail!("tried peeling directory with multiple entries");
                }

                result = peeled;
            }

            Ok(result.value)
        }
        Recipe::Get { directory, path } => {
            let artifact = bake(brioche, *directory, &scope).await?;
            let Artifact::Directory(directory) = artifact.value else {
                anyhow::bail!("tried getting item from non-directory");
            };

            let Some(result) = directory.get(brioche, &path).await? else {
                anyhow::bail!("path not found in directory: {path:?}");
            };

            Ok(result.value)
        }
        Recipe::Insert {
            directory,
            path,
            recipe,
        } => {
            let (directory, artifact) = tokio::try_join!(bake(brioche, *directory, &scope), {
                let scope = scope.clone();
                async move {
                    match recipe {
                        Some(recipe) => Ok(Some(bake(brioche, *recipe, &scope).await?)),
                        None => Ok(None),
                    }
                }
            })?;

            let Artifact::Directory(mut directory) = directory.value else {
                anyhow::bail!("tried removing item from non-directory artifact");
            };

            directory.insert(brioche, &path, artifact).await?;

            Ok(Artifact::Directory(directory))
        }
        Recipe::SetPermissions { file, executable } => {
            let result = bake(brioche, *file, &scope).await?;
            let Artifact::File(mut file) = result.value else {
                anyhow::bail!("tried setting permissions on non-file");
            };

            if let Some(executable) = executable {
                file.executable = executable;
            }

            Ok(Artifact::File(file))
        }
        Recipe::Sync { recipe } => {
            let result = bake(brioche, *recipe, &scope).await?;
            Ok(result.value)
        }
    }
}

pub async fn create_proxy(brioche: &Brioche, recipe: Recipe) -> anyhow::Result<Recipe> {
    if let Recipe::Proxy { .. } = recipe {
        return Ok(recipe);
    }

    let recipe_hash = recipe.hash();
    crate::recipe::save_recipes(brioche, [recipe]).await?;

    Ok(Recipe::Proxy(ProxyRecipe {
        recipe: recipe_hash,
    }))
}

#[derive(Debug, thiserror::Error)]
struct BakeFailed {
    message: String,
    meta: Arc<Meta>,
}

impl std::fmt::Display for BakeFailed {
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
