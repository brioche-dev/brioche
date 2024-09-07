use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use anyhow::Context as _;
use joinery::JoinableIterator as _;
use sqlx::{Acquire as _, Arguments as _};

use crate::{
    blob::BlobHash,
    project::{Project, ProjectHash, Projects},
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

        let mut recipes = HashMap::new();
        crate::recipe::get_recipes(brioche, unvisited.drain(..), &mut recipes).await?;

        for recipe in recipes.values() {
            unvisited.extend(referenced_recipes(recipe));
            references.blobs.extend(referenced_blobs(recipe));
        }

        references.recipes.extend(recipes);
    }

    Ok(())
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectReferences {
    pub recipes: RecipeReferences,
    pub projects: HashMap<ProjectHash, Arc<Project>>,
    pub loaded_blobs: HashMap<BlobHash, Arc<Vec<u8>>>,
}

pub async fn project_references(
    brioche: &Brioche,
    projects: &Projects,
    references: &mut ProjectReferences,
    project_hashes: impl IntoIterator<Item = ProjectHash>,
) -> anyhow::Result<()> {
    let mut unvisited = VecDeque::from_iter(project_hashes);
    let mut new_recipes = HashSet::<RecipeHash>::new();

    loop {
        unvisited.retain(|project_hash| !references.projects.contains_key(project_hash));

        let Some(project_hash) = unvisited.pop_front() else {
            break;
        };

        let project = projects.project(project_hash)?;
        let Project {
            definition: _,
            dependencies,
            modules,
            statics,
        } = &*project;

        references.projects.insert(project_hash, project.clone());
        unvisited.extend(dependencies.values().copied());

        for (module_path, module_file_id) in modules {
            let blob_hash = module_file_id.as_blob_hash()?;

            let contents = brioche
                .vfs
                .read(*module_file_id)?
                .with_context(|| format!("failed to read module file {module_path}"))?;

            references.loaded_blobs.insert(blob_hash, contents);
        }

        for (module_path, module_statics) in statics {
            for (static_, output) in module_statics {
                let output = output
                    .as_ref()
                    .with_context(|| format!("static not loaded for module {module_path}"))?;
                let static_recipe_hash = static_.output_recipe_hash(output)?;
                new_recipes.insert(static_recipe_hash);
            }
        }
    }

    recipe_references(brioche, &mut references.recipes, new_recipes).await?;

    Ok(())
}

pub fn referenced_blobs(recipe: &Recipe) -> Vec<BlobHash> {
    match recipe {
        Recipe::File { content_blob, .. } => vec![*content_blob],
        Recipe::Directory(_)
        | Recipe::Symlink { .. }
        | Recipe::Download(_)
        | Recipe::Unarchive(_)
        | Recipe::Process(_)
        | Recipe::CompleteProcess(_)
        | Recipe::CreateFile { .. }
        | Recipe::CreateDirectory(_)
        | Recipe::Cast { .. }
        | Recipe::Merge { .. }
        | Recipe::Peel { .. }
        | Recipe::Get { .. }
        | Recipe::Insert { .. }
        | Recipe::Glob { .. }
        | Recipe::SetPermissions { .. }
        | Recipe::Proxy(_)
        | Recipe::CollectReferences { .. }
        | Recipe::Sync { .. } => vec![],
    }
}

pub fn referenced_recipes(recipe: &Recipe) -> Vec<RecipeHash> {
    match recipe {
        Recipe::File {
            resources,
            content_blob: _,
            executable: _,
        } => referenced_recipes(resources),
        Recipe::Directory(directory) => directory.entry_hashes().values().copied().collect(),
        Recipe::Symlink { .. } => vec![],
        Recipe::Download(_) => vec![],
        Recipe::Unarchive(unarchive) => referenced_recipes(&unarchive.file),
        Recipe::Process(process) => {
            let ProcessRecipe {
                command,
                args,
                env,
                dependencies,
                work_dir,
                output_scaffold,
                platform: _,
                is_unsafe: _,
                networking: _,
            } = process;

            let templates = [command].into_iter().chain(args).chain(env.values());

            templates
                .flat_map(|template| &template.components)
                .flat_map(|component| match component {
                    ProcessTemplateComponent::Input { recipe } => referenced_recipes(recipe),
                    ProcessTemplateComponent::Literal { .. }
                    | ProcessTemplateComponent::OutputPath
                    | ProcessTemplateComponent::ResourceDir
                    | ProcessTemplateComponent::InputResourceDirs
                    | ProcessTemplateComponent::HomeDir
                    | ProcessTemplateComponent::WorkDir
                    | ProcessTemplateComponent::TempDir => vec![],
                })
                .chain(
                    dependencies
                        .iter()
                        .flat_map(|dep| referenced_recipes(&dep.value)),
                )
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
                is_unsafe: _,
                networking: _,
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
                    | CompleteProcessTemplateComponent::ResourceDir
                    | CompleteProcessTemplateComponent::InputResourceDirs
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
        Recipe::Glob {
            directory,
            patterns: _,
        } => referenced_recipes(directory),
        Recipe::SetPermissions {
            file,
            executable: _,
        } => referenced_recipes(file),
        Recipe::Proxy(proxy) => vec![proxy.recipe],
        Recipe::CollectReferences { recipe } => referenced_recipes(recipe),
        Recipe::Sync { recipe } => referenced_recipes(recipe),
    }
}

pub async fn descendent_artifact_blobs(
    brioche: &Brioche,
    artifacts: impl IntoIterator<Item = Artifact>,
    blobs: &mut HashSet<BlobHash>,
) -> anyhow::Result<()> {
    let mut visited = HashSet::new();
    let mut unvisited = VecDeque::from_iter(artifacts);

    while let Some(artifact) = unvisited.pop_front() {
        let not_visited = visited.insert(artifact.hash());
        if !not_visited {
            continue;
        }

        match artifact {
            Artifact::File(crate::recipe::File {
                content_blob,
                executable: _,
                resources,
            }) => {
                blobs.insert(content_blob);
                unvisited.push_back(Artifact::Directory(resources));
            }
            Artifact::Symlink { .. } => {}
            Artifact::Directory(directory) => {
                let entries = directory.entries(brioche).await?;
                unvisited.extend(entries.into_values());
            }
        }
    }

    Ok(())
}

pub async fn descendent_project_bakes(
    brioche: &Brioche,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<Vec<(Recipe, Artifact)>> {
    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    // Find all recipes baked by the project (either directly in the
    // `project_resolves` table or indirectly in the `child_resolves` table),
    // then filter it down to only `complete_process`, `download`, and `sync`
    // recipes.
    let project_hash_value = project_hash.to_string();
    let project_descendent_bakes = sqlx::query!(
        r#"
            WITH RECURSIVE project_descendent_bakes (recipe_hash) AS (
                SELECT project_bakes.recipe_hash
                FROM project_bakes
                WHERE project_hash = ? AND export = ?
                UNION
                SELECT child_bakes.recipe_hash
                FROM child_bakes
                INNER JOIN project_descendent_bakes ON
                    project_descendent_bakes.recipe_hash = child_bakes.parent_hash
            )
            SELECT
                input_recipes.recipe_hash AS input_hash,
                input_recipes.recipe_json AS input_json,
                output_artifacts.recipe_hash AS output_hash,
                output_artifacts.recipe_json AS output_json
            FROM project_descendent_bakes
            INNER JOIN bakes ON
                bakes.input_hash = project_descendent_bakes.recipe_hash
            INNER JOIN recipes AS input_recipes ON
                input_recipes.recipe_hash = bakes.input_hash
            INNER JOIN recipes AS output_artifacts ON
                output_artifacts.recipe_hash = bakes.output_hash
            WHERE input_recipes.recipe_json->>'type' IN (
                'process',
                'complete_process',
                'download',
                'sync'
            );
        "#,
        project_hash_value,
        export,
    )
    .fetch_all(&mut *db_transaction)
    .await?;

    let project_descendent_bakes = project_descendent_bakes
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

    Ok(project_descendent_bakes)
}

pub async fn local_recipes(
    brioche: &Brioche,
    recipes: impl IntoIterator<Item = RecipeHash>,
) -> anyhow::Result<HashSet<RecipeHash>> {
    let recipes = recipes.into_iter().collect::<Vec<_>>();

    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    // Fetch recipes in batches to avoid hitting the maximum number of
    // SQLite variables per query
    let mut known_recipes = HashSet::new();
    for recipe_batch in recipes.chunks(900) {
        let mut arguments = sqlx::sqlite::SqliteArguments::default();
        for recipe_hash in recipe_batch {
            arguments.add(recipe_hash.to_string());
        }

        let placeholders = std::iter::repeat("?")
            .take(recipe_batch.len())
            .join_with(", ");

        let batch_known_recipes = sqlx::query_as_with::<_, (String,), _>(
            &format!(
                r#"
                    SELECT recipe_hash
                    FROM recipes
                    WHERE recipe_hash IN ({placeholders})
                "#,
            ),
            arguments,
        )
        .fetch_all(&mut *db_transaction)
        .await?;

        for (recipe_hash,) in batch_known_recipes {
            let recipe_hash = recipe_hash.parse()?;
            known_recipes.insert(recipe_hash);
        }
    }

    db_transaction.commit().await?;

    Ok(known_recipes)
}
