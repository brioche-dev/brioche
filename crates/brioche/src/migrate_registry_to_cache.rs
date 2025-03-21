use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use brioche_core::{
    Brioche,
    blob::BlobHash,
    project::{
        DependencyRef, Lockfile, Project, ProjectHash, Projects,
        analyze::{GitRefOptions, StaticOutput, StaticOutputKind, StaticQuery},
    },
    recipe::{Artifact, Recipe, RecipeHash},
    utils::DisplayDuration,
};
use clap::Parser;
use tokio::io::AsyncWriteExt as _;

#[derive(Debug, Parser)]
pub struct MigrateRegistryToCacheArgs {
    #[clap(long)]
    source_dir: PathBuf,

    #[clap(long)]
    data_dir: PathBuf,

    #[clap(long)]
    cache_dir: PathBuf,
}

#[expect(clippy::print_stdout)]
pub async fn migrate_registry_to_cache(args: MigrateRegistryToCacheArgs) -> anyhow::Result<()> {
    let (reporter, _guard) = brioche_core::reporter::console::start_console_reporter(
        brioche_core::reporter::console::ConsoleReporterKind::Plain,
    )?;

    let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
        .data_dir(args.data_dir)
        .registry_client(brioche_core::registry::RegistryClient::disabled())
        .cache_client(brioche_core::cache::CacheClient {
            store: Some(Arc::new(
                object_store::local::LocalFileSystem::new_with_prefix(args.cache_dir)?,
            )),
            writable: true,
            ..Default::default()
        })
        .build()
        .await?;
    crate::start_shutdown_handler(brioche.clone());

    let start = std::time::Instant::now();
    println!("Loading migration content...");

    let local_blob_temp_dir = brioche.data_dir.join("blobs-temp");
    let local_blob_dir = brioche.data_dir.join("blobs");

    tokio::fs::create_dir_all(&local_blob_temp_dir).await?;
    tokio::fs::create_dir_all(&local_blob_dir).await?;

    let context = tokio::task::spawn_blocking(move || {
        let mut bakes = BTreeMap::new();

        let mut bakes_reader = csv::Reader::from_path(args.source_dir.join("bakes.csv"))?;
        for row in bakes_reader.deserialize() {
            let row: BakeRecord = row?;
            if row.is_canonical == "t" {
                bakes.insert(row.input_hash, row.output_hash);
            }
        }

        let mut artifacts = BTreeMap::new();
        let mut num_invalid_recipes = 0;

        let mut recipes_reader = csv::Reader::from_path(args.source_dir.join("recipes.csv"))?;
        for row in recipes_reader.deserialize() {
            let row: RecipeRecord = row?;
            let recipe = serde_json::from_str::<Recipe>(&row.recipe_json);
            let Ok(recipe) = recipe else {
                num_invalid_recipes += 1;
                continue;
            };

            let Ok(artifact) = Artifact::try_from(recipe) else {
                continue;
            };
            let artifact_hash = artifact.hash();

            if artifact_hash != row.recipe_hash {
                num_invalid_recipes += 1;
                continue;
            }

            artifacts.insert(artifact_hash, artifact);
        }

        let mut projects = BTreeMap::new();
        let mut num_invalid_projects = 0;

        let mut projects_reader = csv::Reader::from_path(args.source_dir.join("projects.csv"))?;
        for row in projects_reader.deserialize() {
            let row: ProjectRecord = row?;
            let project = serde_json::from_str::<Project>(&row.project_json);
            let Ok(project) = project else {
                num_invalid_projects += 1;
                continue;
            };

            let hash_matches = row.project_hash.validate_matches(&project).is_ok();
            if !hash_matches {
                num_invalid_projects += 1;
                continue;
            }

            projects.insert(row.project_hash, project);
        }

        let num_bakes = bakes.len();
        let num_artifacts = artifacts.len();
        let num_projects = projects.len();
        println!(
            "[{}] Finished reading records: num_bakes={num_bakes} num_artifacts={num_artifacts} num_projects={num_projects} num_invalid_recipes={num_invalid_recipes} num_invalid_projects={num_invalid_projects}",
            DisplayDuration(start.elapsed())
        );

        let mut num_existing_blobs = 0;
        let mut num_new_blobs = 0;
        let blobs_dir = std::fs::read_dir(args.source_dir.join("blobs"))?;
        for blobs_subdir in blobs_dir {
            let blobs_subdir = blobs_subdir?;
            let blob_prefix = blobs_subdir.file_name().into_string().map_err(|_| anyhow::anyhow!("invalid blob subdir"))?;

            let blobs_subdir = std::fs::read_dir(blobs_subdir.path())?;
            for blob_file in blobs_subdir {
                let blob_file = blob_file?;
                let blob_suffix_with_ext = blob_file.file_name().into_string().map_err(|_| anyhow::anyhow!("invalid blob filename"))?;
                let blob_suffix = blob_suffix_with_ext.strip_suffix(".zst").context("invalid blob extension")?;
                let blob_hash: BlobHash = format!("{blob_prefix}{blob_suffix}").parse()?;

                let local_blob_path = local_blob_dir.join(blob_hash.to_string());
                if local_blob_path.exists() {
                    num_existing_blobs += 1;
                    continue;
                }

                let blob_reader = std::fs::File::open(blob_file.path())?;
                let mut blob_reader = zstd_framed::ZstdReader::builder(blob_reader).build()?;

                let local_blob_temp_path = local_blob_temp_dir.join(ulid::Ulid::new().to_string());
                let mut local_blob_writer = std::fs::File::create(&local_blob_temp_path)?;
                std::io::copy(&mut blob_reader, &mut local_blob_writer)?;
                local_blob_writer.set_modified(brioche_epoch())?;
                drop(local_blob_writer);

                std::fs::rename(&local_blob_temp_path, &local_blob_path)?;
                num_new_blobs += 1;
            }
        }

        println!("[{}] Finished reading blobs: num_new_blobs={num_new_blobs}, num_existing_blobs={num_existing_blobs}", DisplayDuration(start.elapsed()));

        anyhow::Ok(MigrationContext {
            artifacts,
            bakes,
            projects,
        })
    })
    .await??;

    let num_saved = brioche_core::recipe::save_recipes(
        &brioche,
        context
            .artifacts
            .values()
            .map(|artifact| Recipe::from(artifact.clone())),
    )
    .await?;

    println!(
        "[{}] Finished saving artifacts num_saved={num_saved}",
        DisplayDuration(start.elapsed())
    );

    let local_project_dir = brioche.data_dir.join("projects");
    let local_project_temp_dir = brioche.data_dir.join("projects-temp");

    tokio::fs::create_dir_all(&local_project_dir).await?;
    tokio::fs::create_dir_all(&local_project_temp_dir).await?;

    let mut num_existing_projects = 0;
    let mut num_new_projects = 0;
    let mut num_invalid_projects = 0;

    let mut local_projects = BTreeMap::new();
    for (project_hash, project) in &context.projects {
        let local_project_path = local_project_dir.join(project_hash.to_string());
        if tokio::fs::try_exists(&local_project_path).await? {
            num_existing_projects += 1;
            local_projects.insert(project_hash, local_project_path);
            continue;
        }

        let local_project_temp_path = local_project_temp_dir.join(ulid::Ulid::new().to_string());
        tokio::fs::create_dir_all(&local_project_temp_path).await?;

        let did_write = save_project(&brioche, &context, &local_project_temp_path, project)
            .await
            .with_context(|| format!("failed to save project {project_hash}"))?;

        if !did_write {
            tokio::fs::remove_dir_all(&local_project_temp_path).await?;
            num_invalid_projects += 1;
            continue;
        }

        tokio::fs::rename(&local_project_temp_path, &local_project_path).await?;
        local_projects.insert(project_hash, local_project_path);
        num_new_projects += 1;
    }

    println!(
        "[{}] Finished writing projects num_new_projects={num_new_projects} num_existing_projects={num_existing_projects} num_invalid_projects={num_invalid_projects}",
        DisplayDuration(start.elapsed())
    );

    let projects = Projects::default();
    let mut loaded_projects = BTreeSet::new();

    for (project_hash, path) in local_projects {
        let actual_project_hash = projects
            .load(
                &brioche,
                &path,
                brioche_core::project::ProjectValidation::Standard,
                brioche_core::project::ProjectLocking::Locked,
            )
            .await?;

        if actual_project_hash != *project_hash {
            anyhow::bail!("expected project hash {project_hash}, was {actual_project_hash}");
        }

        loaded_projects.insert(*project_hash);
    }

    println!(
        "[{}] Finished loading projects",
        DisplayDuration(start.elapsed())
    );

    println!("Saving content to cache...");

    let mut saved_artifacts = HashSet::new();
    let mut num_existing_artifacts = 0;
    let mut num_new_artifacts = 0;

    let mut num_existing_bakes = 0;
    let mut num_new_bakes = 0;
    let mut num_invalid_bakes = 0;
    for (input_hash, output_hash) in context.bakes {
        let should_save_artifact = saved_artifacts.insert(output_hash);
        if should_save_artifact {
            let artifact = context.artifacts.get(&output_hash);
            let Some(artifact) = artifact else {
                num_invalid_bakes += 1;
                continue;
            };

            let did_save_artifact =
                brioche_core::cache::save_artifact(&brioche, artifact.clone()).await?;
            if did_save_artifact {
                num_existing_artifacts += 1;
            } else {
                num_new_artifacts += 1;
            }
        }

        let did_save_bake =
            brioche_core::cache::save_bake(&brioche, input_hash, output_hash).await?;
        if did_save_bake {
            num_existing_bakes += 1;
        } else {
            num_new_bakes += 1;
        }
    }

    println!(
        "[{}] Saved bakes num_existing_bakes={num_existing_bakes} num_new_bakes={num_new_bakes} num_invalid_bakes={num_invalid_bakes}",
        DisplayDuration(start.elapsed())
    );

    let mut num_new_projects = 0;
    let mut num_existing_projects = 0;
    for project_hash in loaded_projects {
        let project_artifact = brioche_core::project::artifact::create_artifact_with_projects(
            &brioche,
            &projects,
            &[project_hash],
        )
        .await?;
        let project_artifact = Artifact::Directory(project_artifact);
        let project_artifact_hash = project_artifact.hash();
        let should_save_artifact = saved_artifacts.insert(project_artifact_hash);

        if should_save_artifact {
            let did_save_artifact =
                brioche_core::cache::save_artifact(&brioche, project_artifact.clone()).await?;
            if did_save_artifact {
                num_existing_artifacts += 1;
            } else {
                num_new_artifacts += 1;
            }
        }

        let did_save_project = brioche_core::cache::save_project_artifact_hash(
            &brioche,
            project_hash,
            project_artifact_hash,
        )
        .await?;
        if did_save_project {
            num_new_projects += 1;
        } else {
            num_existing_projects += 1;
        }
    }

    println!(
        "[{}] Saved projects num_new_projects={num_new_projects} num_existing_projects={num_existing_projects}",
        DisplayDuration(start.elapsed())
    );

    println!(
        "[{}] Finished migrating registry to cache num_new_artifacts={num_new_artifacts} num_existing_artifacts={num_existing_artifacts}",
        DisplayDuration(start.elapsed())
    );

    Ok(())
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct BakeRecord {
    id: usize,
    input_hash: RecipeHash,
    output_hash: RecipeHash,
    is_canonical: String,
    created_at: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RecipeRecord {
    recipe_hash: RecipeHash,
    recipe_json: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ProjectRecord {
    project_hash: ProjectHash,
    project_json: String,
    created_at: String,
}

struct MigrationContext {
    artifacts: BTreeMap<RecipeHash, Artifact>,
    bakes: BTreeMap<RecipeHash, RecipeHash>,
    projects: BTreeMap<ProjectHash, Project>,
}

pub fn brioche_epoch() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800)
}

async fn save_project(
    brioche: &Brioche,
    context: &MigrationContext,
    path: &Path,
    project: &Project,
) -> anyhow::Result<bool> {
    for (module_path, statics) in &project.statics {
        for (static_, output) in statics {
            let Some(output) = output else {
                continue;
            };

            let recipe_hash = static_.output_recipe_hash(output)?;

            let module_path = module_path.to_logical_path(path);
            let module_dir = module_path.parent().context("no parent dir for module")?;

            match static_ {
                StaticQuery::Include(include) => {
                    let recipe_hash = recipe_hash.context("no recipe hash for include static")?;
                    let artifact = context
                        .artifacts
                        .get(&recipe_hash)
                        .context("artifact not found for project")?;
                    let include_path = module_dir.join(include.path());
                    brioche_core::output::create_output(
                        brioche,
                        artifact,
                        brioche_core::output::OutputOptions {
                            link_locals: false,
                            merge: true,
                            mtime: None,
                            output_path: &include_path,
                            resource_dir: None,
                        },
                    )
                    .await?;
                }
                StaticQuery::Glob { .. } => {
                    let recipe_hash = recipe_hash.context("no recipe hash for glob static")?;
                    let artifact = context
                        .artifacts
                        .get(&recipe_hash)
                        .context("artifact not found for project")?;
                    brioche_core::output::create_output(
                        brioche,
                        artifact,
                        brioche_core::output::OutputOptions {
                            link_locals: false,
                            merge: true,
                            mtime: None,
                            output_path: module_dir,
                            resource_dir: None,
                        },
                    )
                    .await?;
                }
                StaticQuery::Download { .. } | StaticQuery::GitRef { .. } => {
                    // No need to do anything while fetching the project
                }
            }
        }
    }

    for (module_path, file_id) in &project.modules {
        anyhow::ensure!(
            module_path != "brioche.lock",
            "lockfile included as a project module"
        );

        let temp_module_path = module_path.to_logical_path(path);
        anyhow::ensure!(
            temp_module_path.starts_with(path),
            "module path escapes project root",
        );

        let blob_hash = file_id.as_blob_hash()?;
        let module_blob_path = brioche_core::blob::local_blob_path(brioche, blob_hash);
        if let Some(temp_module_dir) = temp_module_path.parent() {
            tokio::fs::create_dir_all(temp_module_dir)
                .await
                .context("failed to create temporary module directory")?;
        }

        if !tokio::fs::try_exists(&module_blob_path).await? {
            return Ok(false);
        }

        tokio::fs::copy(&module_blob_path, &temp_module_path)
            .await
            .with_context(|| {
                format!("failed to copy blob {blob_hash} for module {module_path:?}")
            })?;
    }

    let dependencies = project
        .dependencies
        .iter()
        .map(|(name, dep_ref)| {
            let DependencyRef::Project(dep_hash) = dep_ref else {
                anyhow::bail!("cannot migrate project with circular dependencies");
            };
            anyhow::Ok((name.clone(), *dep_hash))
        })
        .collect::<anyhow::Result<_>>()?;

    let mut downloads = BTreeMap::new();
    let mut git_refs = BTreeMap::new();
    for (static_, output) in project.statics.values().flatten() {
        match static_ {
            StaticQuery::Include(_) | StaticQuery::Glob { .. } => {
                continue;
            }
            StaticQuery::Download { url } => {
                let Some(StaticOutput::Kind(StaticOutputKind::Download { hash })) = output else {
                    continue;
                };

                downloads.insert(url.clone(), hash.clone());
            }
            StaticQuery::GitRef(GitRefOptions { repository, ref_ }) => {
                let Some(StaticOutput::Kind(StaticOutputKind::GitRef { commit })) = output else {
                    continue;
                };

                let repo_refs: &mut BTreeMap<_, _> =
                    git_refs.entry(repository.clone()).or_default();
                repo_refs.insert(ref_.clone(), commit.clone());
            }
        }
    }

    let lockfile = Lockfile {
        dependencies,
        downloads,
        git_refs,
    };
    let lockfile_path = path.join("brioche.lock");
    let lockfile_contents =
        serde_json::to_string_pretty(&lockfile).context("failed to serialize lockfile")?;
    let mut lockfile_file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lockfile_path)
        .await
        .context("failed to create lockfile")?;
    lockfile_file
        .write_all(lockfile_contents.as_bytes())
        .await
        .context("failed to write lockfile")?;

    Ok(true)
}
