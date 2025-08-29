use std::collections::{HashSet, VecDeque};

use crate::recipe::Unarchive;

use super::{Brioche, recipe::Recipe};

/// Returns true if a recipe would be cheap to bake after fetching all of
/// its inputs from a remote cache. This corresponds to the CLI option
/// `--experimental-lazy`.
///
/// In some contexts, `brioche build --sync` is used to sync the build
/// result to a remote cache, but without any need for the build result itself.
/// For this scenario, `brioche build --sync` would still fetch the build
/// result from the cache, only to re-validate that every expensive recipe
/// is already in the cache-- which means fetching from the cache in this
/// situation would do nothing but waste time and bandwidth.
///
/// This function answers the question: "would running `brioche build --sync`
/// just fetch existing bakes from the remote cache without syncing any new
/// recipes?"
pub async fn is_cheap_to_bake_or_in_remote_cache(
    brioche: &Brioche,
    recipe: Recipe,
) -> anyhow::Result<bool> {
    let mut needed_recipes = VecDeque::from_iter([recipe]);
    let mut visited_recipes = HashSet::new();

    while let Some(recipe) = needed_recipes.pop_front() {
        let recipe_hash = recipe.hash();
        if !visited_recipes.insert(recipe_hash) {
            continue;
        }

        match recipe {
            recipe if recipe.is_expensive_to_bake() => {
                tracing::debug!("checking {:?} {}", recipe.kind(), recipe_hash);
                let bake_result = crate::cache::load_bake(brioche, recipe_hash).await?;
                if bake_result.is_none() {
                    // Expensive recipe not found in cache, so we'd need
                    // to bake it
                    return Ok(false);
                }
            }
            Recipe::File {
                content_blob: _,
                executable: _,
                resources,
            }
            | Recipe::CreateFile {
                content: _,
                executable: _,
                resources,
            } => {
                needed_recipes.push_back(resources.value);
            }
            Recipe::Directory(_) | Recipe::Symlink { .. } => {
                // No extra needed recipes
            }
            Recipe::Unarchive(Unarchive {
                file,
                compression: _,
                archive: _,
            })
            | Recipe::SetPermissions {
                file,
                executable: _,
            } => {
                needed_recipes.push_back(file.value);
            }
            Recipe::Download(_)
            | Recipe::Process(_)
            | Recipe::CompleteProcess(_)
            | Recipe::Sync { .. } => {
                // These recipes all should've been caught by the
                // `.is_expensive_to_bake()` check above!
                panic!("reached recipe that should've been expensive to bake");
            }
            Recipe::CreateDirectory(directory) => {
                needed_recipes.extend(directory.entries.into_values().map(|recipe| recipe.value));
            }
            Recipe::Cast { recipe, to: _ }
            | Recipe::AttachResources { recipe }
            | Recipe::CollectReferences { recipe } => {
                needed_recipes.push_back(recipe.value);
            }
            Recipe::Merge { directories } => {
                needed_recipes.extend(directories.into_iter().map(|recipe| recipe.value));
            }
            Recipe::Peel {
                directory,
                depth: _,
            }
            | Recipe::Get { directory, path: _ }
            | Recipe::Glob {
                directory,
                patterns: _,
            } => {
                needed_recipes.push_back(directory.value);
            }
            Recipe::Insert {
                directory,
                path: _,
                recipe,
            } => {
                needed_recipes.push_back(directory.value);
                if let Some(recipe) = recipe {
                    needed_recipes.push_back(recipe.value);
                }
            }
            Recipe::Proxy(proxy) => {
                let inner = proxy.inner(brioche).await?;
                needed_recipes.push_back(inner);
            }
        }
    }

    Ok(true)
}
