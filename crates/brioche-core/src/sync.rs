use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use human_repr::HumanDuration;

use crate::{
    project::ProjectHash,
    references::{ProjectReferences, RecipeReferences},
    Brioche,
};

pub async fn sync_project(
    brioche: &Brioche,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<()> {
    // Get all descendent bakes for the project/export

    let descendent_project_bakes =
        crate::references::descendent_project_bakes(brioche, project_hash, export).await?;

    // Sync all descendent bakes

    sync_bakes(brioche, descendent_project_bakes, true).await?;

    Ok(())
}

pub async fn sync_bakes(
    brioche: &Brioche,
    bakes: Vec<(crate::recipe::Recipe, crate::recipe::Artifact)>,
    verbose: bool,
) -> anyhow::Result<SyncBakesResults> {
    // TODO: Use reporter for logging in this function

    // Collect the references from each input recipe/output artifact

    let start_refs = std::time::Instant::now();

    let mut sync_references = RecipeReferences::default();

    let recipe_hashes = bakes
        .iter()
        .flat_map(|(input, output)| [input.hash(), output.hash()]);
    crate::references::recipe_references(brioche, &mut sync_references, recipe_hashes).await?;

    let num_recipe_refs = sync_references.recipes.len();
    let num_blob_refs = sync_references.blobs.len();
    if verbose {
        println!(
            "Collected refs in {} ({num_recipe_refs} recipes, {num_blob_refs} blobs)",
            start_refs.elapsed().human_duration()
        );
    }

    let sync_recipe_results = sync_recipe_references(brioche, &sync_references, verbose).await?;

    // Sync each baked recipe

    let start_bakes = std::time::Instant::now();

    let num_bakes = bakes.len();

    let all_bakes = bakes
        .into_iter()
        .map(|(input, output)| (input.hash(), output.hash()))
        .collect::<Vec<_>>();
    let known_bakes = brioche.registry_client.known_bakes(&all_bakes).await?;
    let num_new_bakes = all_bakes.len() - known_bakes.len();
    let new_bakes = all_bakes
        .into_iter()
        .filter(|bake| !known_bakes.contains(bake));

    futures::stream::iter(new_bakes)
        .map(Ok)
        .try_for_each_concurrent(Some(25), |(input_hash, output_hash)| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(async move {
                    brioche
                        .registry_client
                        .create_bake(input_hash, output_hash)
                        .await
                })
                .await??;
                anyhow::Ok(())
            }
        })
        .await?;

    if verbose {
        println!(
            "Finished syncing {num_new_bakes} / {num_bakes} bakes in {}",
            start_bakes.elapsed().human_duration()
        );
    }

    Ok(SyncBakesResults {
        num_new_bakes,
        num_new_blobs: sync_recipe_results.num_new_blobs,
        num_new_recipes: sync_recipe_results.num_new_recipes,
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncBakesResults {
    pub num_new_blobs: usize,
    pub num_new_recipes: usize,
    pub num_new_bakes: usize,
}

impl SyncBakesResults {
    pub fn merge(&mut self, other: Self) {
        self.num_new_blobs += other.num_new_blobs;
        self.num_new_recipes += other.num_new_recipes;
        self.num_new_bakes += other.num_new_bakes;
    }
}

pub async fn sync_recipe_references(
    brioche: &Brioche,
    references: &RecipeReferences,
    verbose: bool,
) -> anyhow::Result<SyncRecipeReferencesResult> {
    // Sync referenced blobs

    let start_blobs = std::time::Instant::now();

    let all_blobs = references.blobs.iter().cloned().collect::<Vec<_>>();
    let known_blobs = brioche.registry_client.known_blobs(&all_blobs).await?;
    let num_new_blobs = all_blobs.len() - known_blobs.len();
    let new_blobs = all_blobs
        .into_iter()
        .filter(|blob_hash| !known_blobs.contains(blob_hash));

    futures::stream::iter(new_blobs)
        .map(Ok)
        .try_for_each_concurrent(Some(25), |blob_hash| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(async move {
                    let blob_path = crate::blob::blob_path(&brioche, blob_hash).await?;

                    // TODO: Figure out if we can stream the blob (this
                    // will error out due to `reqwest-retry`)
                    let blob_content = tokio::fs::read(&blob_path)
                        .await
                        .with_context(|| format!("failed to read blob {blob_hash}"))?;
                    brioche
                        .registry_client
                        .send_blob(blob_hash, blob_content)
                        .await?;

                    anyhow::Ok(())
                })
                .await??;

                anyhow::Ok(())
            }
        })
        .await?;

    let num_total_blobs = references.blobs.len();
    if verbose {
        println!(
            "Finished syncing {num_new_blobs} / {num_total_blobs} blobs in {}",
            start_blobs.elapsed().human_duration()
        );
    }

    // Sync referenced recipes

    let start_recipes = std::time::Instant::now();

    let all_recipe_hashes = references.recipes.keys().cloned().collect::<Vec<_>>();
    let known_recipe_hashes = brioche
        .registry_client
        .known_recipes(&all_recipe_hashes)
        .await?;
    let new_recipes: Vec<_> = references
        .recipes
        .clone()
        .into_iter()
        .filter_map(|(hash, recipe)| {
            if known_recipe_hashes.contains(&hash) {
                None
            } else {
                Some(recipe)
            }
        })
        .collect();

    brioche.registry_client.create_recipes(&new_recipes).await?;

    let num_new_recipes = new_recipes.len();
    let num_total_recipes = references.recipes.len();
    if verbose {
        println!(
            "Finished syncing {num_new_recipes} / {num_total_recipes} recipes in {}",
            start_recipes.elapsed().human_duration()
        );
    }

    Ok(SyncRecipeReferencesResult {
        num_new_blobs,
        num_new_recipes,
    })
}

pub struct SyncRecipeReferencesResult {
    pub num_new_blobs: usize,
    pub num_new_recipes: usize,
}

pub async fn sync_project_references(
    brioche: &Brioche,
    references: &ProjectReferences,
    verbose: bool,
) -> anyhow::Result<SyncProjectReferencesResult> {
    // Sync referenced blobs and recipes

    let recipe_result = sync_recipe_references(brioche, &references.recipes, verbose).await?;

    // Sync loaded blobs

    let start_blobs = std::time::Instant::now();

    let all_blobs = references.loaded_blobs.keys().cloned().collect::<Vec<_>>();

    // TODO: For some reason, this API call often times out (or hangs forever
    // if one isn't set), but will work very quickly after retrying. We should
    // figure out why this is happening
    let known_blobs = brioche.registry_client.known_blobs(&all_blobs).await?;

    let num_new_blobs = all_blobs.len() - known_blobs.len();
    let new_blobs = references
        .loaded_blobs
        .clone()
        .into_iter()
        .filter(|(blob_hash, _)| !known_blobs.contains(blob_hash));

    futures::stream::iter(new_blobs)
        .map(Ok)
        .try_for_each_concurrent(Some(25), |(blob_hash, blob_content)| {
            let brioche = brioche.clone();
            async move {
                tokio::spawn(async move {
                    brioche
                        .registry_client
                        .send_blob(blob_hash, (*blob_content).clone())
                        .await?;

                    anyhow::Ok(())
                })
                .await??;

                anyhow::Ok(())
            }
        })
        .await?;

    let num_total_blobs = references.loaded_blobs.len();
    if verbose {
        println!(
            "Finished syncing {num_new_blobs} / {num_total_blobs} loaded blobs in {}",
            start_blobs.elapsed().human_duration()
        );
    }

    // Sync referenced projects

    let start_projects = std::time::Instant::now();

    let all_project_hashes = references.projects.keys().cloned().collect::<Vec<_>>();
    let known_project_hashes = brioche
        .registry_client
        .known_projects(&all_project_hashes)
        .await?;
    let mut new_projects = references.projects.clone();
    for project_hash in known_project_hashes {
        new_projects.remove(&project_hash);
    }

    brioche
        .registry_client
        .create_projects(&new_projects)
        .await?;

    let num_new_projects = new_projects.len();
    let num_total_projects = references.projects.len();
    if verbose {
        println!(
            "Finished syncing {num_new_projects} / {num_total_projects} projects in {}",
            start_projects.elapsed().human_duration()
        );
    }

    Ok(SyncProjectReferencesResult {
        num_new_blobs: num_new_blobs + recipe_result.num_new_blobs,
        num_new_recipes: recipe_result.num_new_recipes,
        num_new_projects,
    })
}

pub struct SyncProjectReferencesResult {
    pub num_new_blobs: usize,
    pub num_new_recipes: usize,
    pub num_new_projects: usize,
}
