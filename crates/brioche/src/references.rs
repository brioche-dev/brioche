use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Context as _;
use sqlx::Acquire as _;

use crate::{
    blob::BlobHash,
    project::ProjectHash,
    recipe::{
        Artifact, CompleteProcessRecipe, CompleteProcessTemplateComponent, ProcessRecipe,
        ProcessTemplateComponent, Recipe, RecipeHash,
    },
    Brioche,
};

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecipeReferences {
    pub blobs: HashSet<BlobHash>,
    pub recipes: HashMap<RecipeHash, Recipe>,
}

pub async fn recipe_references(
    brioche: &Brioche,
    references: &mut RecipeReferences,
    recipes: impl IntoIterator<Item = RecipeHash>,
) -> anyhow::Result<()> {
    let mut unvisited = VecDeque::from_iter(recipes);

    loop {
        unvisited.retain(|recipe| !references.recipes.contains_key(recipe));

        if unvisited.is_empty() {
            break;
        }

        let recipes = crate::recipe::get_recipes(brioche, unvisited.drain(..)).await?;

        for recipe in recipes.values() {
            unvisited.extend(referenced_recipes(recipe));
            references.blobs.extend(referenced_blobs(recipe));
        }

        references.recipes.extend(recipes);
    }

    Ok(())
}

pub fn referenced_blobs(recipe: &Recipe) -> Vec<BlobHash> {
    match recipe {
        Recipe::File { content_blob, .. } => vec![*content_blob],
        Recipe::Directory(_)
        | Recipe::Symlink { .. }
        | Recipe::Download(_)
        | Recipe::Unpack(_)
        | Recipe::Process(_)
        | Recipe::CompleteProcess(_)
        | Recipe::CreateFile { .. }
        | Recipe::CreateDirectory(_)
        | Recipe::Cast { .. }
        | Recipe::Merge { .. }
        | Recipe::Peel { .. }
        | Recipe::Get { .. }
        | Recipe::Insert { .. }
        | Recipe::SetPermissions { .. }
        | Recipe::Proxy(_) => vec![],
    }
}

pub fn referenced_recipes(recipe: &Recipe) -> Vec<RecipeHash> {
    match recipe {
        Recipe::File {
            resources,
            content_blob: _,
            executable: _,
        } => referenced_recipes(resources),
        Recipe::Directory(directory) => directory
            .entry_hashes()
            .values()
            .map(|entry| entry.value)
            .collect(),
        Recipe::Symlink { .. } => vec![],
        Recipe::Download(_) => vec![],
        Recipe::Unpack(unpack) => referenced_recipes(&unpack.file),
        Recipe::Process(process) => {
            let ProcessRecipe {
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
                    ProcessTemplateComponent::Input { recipe } => referenced_recipes(recipe),
                    ProcessTemplateComponent::Literal { .. }
                    | ProcessTemplateComponent::OutputPath
                    | ProcessTemplateComponent::ResourcesDir
                    | ProcessTemplateComponent::HomeDir
                    | ProcessTemplateComponent::WorkDir
                    | ProcessTemplateComponent::TempDir => vec![],
                })
                .chain(referenced_recipes(work_dir))
                .chain(
                    output_scaffold
                        .iter()
                        .flat_map(|recipe| referenced_recipes(recipe)),
                )
                .collect()
        }
        Recipe::CompleteProcess(process) => {
            let CompleteProcessRecipe {
                command,
                args,
                env,
                work_dir,
                output_scaffold,
                platform: _,
            } = process;

            let work_dir = Recipe::from(work_dir.clone());
            let output_scaffold = output_scaffold
                .as_ref()
                .map(|artifact| Recipe::from((**artifact).clone()));

            let templates = [command].into_iter().chain(args).chain(env.values());

            templates
                .flat_map(|template| &template.components)
                .flat_map(|component| match component {
                    CompleteProcessTemplateComponent::Input { artifact } => {
                        let recipe = Recipe::from(artifact.value.clone());
                        referenced_recipes(&recipe)
                    }
                    CompleteProcessTemplateComponent::Literal { .. }
                    | CompleteProcessTemplateComponent::OutputPath
                    | CompleteProcessTemplateComponent::ResourcesDir
                    | CompleteProcessTemplateComponent::HomeDir
                    | CompleteProcessTemplateComponent::WorkDir
                    | CompleteProcessTemplateComponent::TempDir => vec![],
                })
                .chain(referenced_recipes(&work_dir))
                .chain(output_scaffold.iter().flat_map(referenced_recipes))
                .collect()
        }
        Recipe::CreateFile {
            content: _,
            executable: _,
            resources,
        } => referenced_recipes(resources),
        Recipe::CreateDirectory(directory) => directory
            .entries
            .values()
            .flat_map(|entry| referenced_recipes(entry))
            .collect(),
        Recipe::Cast { recipe, to: _ } => referenced_recipes(recipe),
        Recipe::Merge { directories } => directories
            .iter()
            .flat_map(|dir| referenced_recipes(dir))
            .collect(),
        Recipe::Peel {
            directory,
            depth: _,
        } => referenced_recipes(directory),
        Recipe::Get { directory, path: _ } => referenced_recipes(directory),
        Recipe::Insert {
            directory,
            path: _,
            recipe,
        } => referenced_recipes(directory)
            .into_iter()
            .chain(recipe.iter().flat_map(|recipe| referenced_recipes(recipe)))
            .collect(),
        Recipe::SetPermissions {
            file,
            executable: _,
        } => referenced_recipes(file),
        Recipe::Proxy(proxy) => vec![proxy.recipe],
    }
}

pub async fn descendent_project_resolves(
    brioche: &Brioche,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<Vec<(Recipe, Artifact)>> {
    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    // Find all recipes resolved by the project (either directly in the
    // `project_resolves` table or indirectly in the `child_resolves` table),
    // then filter it down to only `complete_process` and `download` recipes.
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
                .context("invalid recipe hash from database")?;
            let output_hash = record
                .output_hash
                .parse()
                .context("invalid recipe hash from database")?;
            let input: Recipe = serde_json::from_str(&record.input_json)
                .context("invalid recipe JSON from database")?;
            let output: Recipe = serde_json::from_str(&record.output_json)
                .context("invalid recipe JSON from database")?;
            let output: Artifact = output
                .try_into()
                .map_err(|_| anyhow::anyhow!("output recipe is not complete"))?;

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
