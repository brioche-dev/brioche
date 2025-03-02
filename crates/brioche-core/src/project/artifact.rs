use std::{
    collections::{HashSet, VecDeque},
    path::Path,
};

use anyhow::Context as _;
use bstr::ByteSlice as _;

use crate::{
    Brioche,
    recipe::{Artifact, ArtifactDiscriminants, Directory},
};

use super::{ProjectHash, Projects, analyze::StaticQuery};

pub async fn create_artifact_with_projects(
    brioche: &Brioche,
    projects: &Projects,
    project_hashes: &[ProjectHash],
) -> anyhow::Result<Directory> {
    let mut artifact = Directory::default();

    let mut written_projects = HashSet::new();
    let mut queued_projects = VecDeque::from_iter(project_hashes.iter().copied());
    let mut permit = crate::blob::get_save_blob_permit().await?;

    while let Some(project_hash) = queued_projects.pop_front() {
        // Ensure we only add each project once
        if !written_projects.insert(project_hash) {
            continue;
        }

        let mut project_artifact = Directory::default();

        // Get the project
        let project = projects
            .project(project_hash)
            .with_context(|| format!("project {project_hash} not loaded"))?;

        // Enqueue each dependency so they get added to the artifact
        queued_projects.extend(project.dependency_hashes());

        // Add statics to the artifact
        for (module_path, statics) in &project.statics {
            for (static_, output) in statics {
                let Some(output) = output else {
                    continue;
                };

                let recipe_hash = static_.output_recipe_hash(output)?;

                let module_dir = module_path.parent().context("no parent dir for module")?;

                match static_ {
                    StaticQuery::Include(include) => {
                        let recipe_hash =
                            recipe_hash.context("no recipe hash for include static")?;
                        let recipe = crate::recipe::get_recipe(brioche, recipe_hash).await?;
                        let artifact: crate::recipe::Artifact =
                            recipe.try_into().map_err(|_| {
                                anyhow::anyhow!("included static recipe is not an artifact")
                            })?;

                        let include_path = module_dir.join(include.path());
                        insert_non_conflicting(
                            brioche,
                            &mut project_artifact,
                            include_path.as_str().as_bytes(),
                            &artifact,
                        )
                        .await?;
                    }
                    StaticQuery::Glob { .. } => {
                        let recipe_hash = recipe_hash.context("no recipe hash for glob static")?;
                        let recipe = crate::recipe::get_recipe(brioche, recipe_hash).await?;
                        let artifact: crate::recipe::Artifact =
                            recipe.try_into().map_err(|_| {
                                anyhow::anyhow!("included static recipe is not an artifact")
                            })?;

                        insert_merge(
                            brioche,
                            &mut project_artifact,
                            module_dir.as_str().as_bytes(),
                            &artifact,
                        )
                        .await?;
                    }
                    StaticQuery::Download { .. } | StaticQuery::GitRef { .. } => {
                        // Nothing to add
                    }
                }
            }
        }

        // Add each module to the artifact
        for (module_path, file_id) in &project.modules {
            let content_blob = crate::vfs::commit_blob(brioche, *file_id).await?;

            let module_artifact = Artifact::File(crate::recipe::File {
                content_blob,
                executable: false,
                resources: Directory::default(),
            });

            insert_non_conflicting(
                brioche,
                &mut project_artifact,
                module_path.as_str().as_bytes(),
                &module_artifact,
            )
            .await?;
        }

        // Add the lockfile to the artifact
        let lockfile = super::project_lockfile(&project);
        let lockfile_contents =
            serde_json::to_string_pretty(&lockfile).context("failed to serialize lockfile")?;

        let lockfile_blob = crate::blob::save_blob(
            brioche,
            &mut permit,
            lockfile_contents.as_bytes(),
            crate::blob::SaveBlobOptions::default(),
        )
        .await?;
        let lockfile_artifact = Artifact::File(crate::recipe::File {
            content_blob: lockfile_blob,
            executable: false,
            resources: Directory::default(),
        });

        insert_non_conflicting(
            brioche,
            &mut project_artifact,
            b"brioche.lock",
            &lockfile_artifact,
        )
        .await?;

        // Add the project to the root artifact
        let project_path = project_hash.to_string();
        insert_non_conflicting(
            brioche,
            &mut artifact,
            project_path.as_bytes(),
            &Artifact::Directory(project_artifact),
        )
        .await?;
    }

    Ok(artifact)
}

pub async fn save_projects_from_artifact(
    brioche: &Brioche,
    projects: &Projects,
    artifact: &Directory,
) -> anyhow::Result<HashSet<ProjectHash>> {
    let mut loaded_projects = HashSet::new();
    let mut unloaded_projects = vec![];

    // Iterate through each entry in the directory. Each one should be a
    // project directory named after its hash
    let entries = artifact.entries(brioche).await?;
    for (name, entry) in entries {
        let name = name
            .to_str()
            .with_context(|| format!("path {name} is not valid UTF-8"))?;
        let project_hash: ProjectHash = name
            .parse()
            .with_context(|| format!("path {name} is not a valid project hash"))?;
        let Artifact::Directory(entry) = entry else {
            anyhow::bail!("expected path {name} to be a directory");
        };

        let local_path = brioche
            .data_dir
            .join("projects")
            .join(project_hash.to_string());

        if projects.project(project_hash).is_ok() {
            // Project has already been loaded, so no need to save it
            // from the artifact
            loaded_projects.insert(project_hash);
        } else {
            // Project has not been loaded, so queue it to be loaded
            unloaded_projects.push((project_hash, local_path, entry));
        }
    }

    let mut ready_paths = vec![];
    let mut needed_paths = vec![];

    for (project_hash, local_path, artifact) in unloaded_projects {
        let local_project_hash = super::load::local_project_hash(brioche, &local_path).await?;
        if local_project_hash == Some(project_hash) {
            // Project already exists on disk and matches the expected hash,
            // so we can load it from the local directory
            ready_paths.push((project_hash, local_path));
        } else {
            // Project does not exist (or has a different hash), so we
            // should save it from the artifact
            needed_paths.push((project_hash, local_path, artifact));
        }
    }

    if !needed_paths.is_empty() {
        // We have some projects to save from the artifact

        // We store projects under `projects/inner/{hash}`, then create
        // symlinks once all projects have been saved. This ensures that, if
        // `projects/{hash}` exists, we know all of its dependencies have
        // been saved
        let inner_dir_path = brioche.data_dir.join("projects").join("inner");
        tokio::fs::create_dir_all(&inner_dir_path).await?;

        let mut new_symlinks = vec![];

        for (project_hash, local_path, entry) in needed_paths {
            let inner_path = inner_dir_path.join(project_hash.to_string());

            // Remove any partially-written project files if they exist
            let _ = tokio::fs::remove_dir_all(&inner_path).await;

            // Write the project to the inner directory
            crate::output::create_output(
                brioche,
                &Artifact::Directory(entry),
                crate::output::OutputOptions {
                    output_path: &inner_path,
                    resource_dir: None,
                    merge: false,
                    mtime: None,
                    link_locals: true,
                },
            )
            .await?;

            // Queue up the symlink to create
            new_symlinks.push((
                project_hash,
                local_path,
                Path::new("inner").join(project_hash.to_string()),
            ));
        }

        // Create each symlink
        for (project_hash, local_path, target_path) in new_symlinks {
            match tokio::fs::symlink(target_path, &local_path).await {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error.into());
                }
            }

            ready_paths.push((project_hash, local_path));
        }
    }

    // Load each unloaded project from the local path
    for (expected_project_hash, local_path) in ready_paths {
        let project_hash = projects
            .load(
                brioche,
                &local_path,
                crate::project::ProjectValidation::Standard,
                crate::project::ProjectLocking::Locked,
            )
            .await?;

        // Validate that the project hash matches
        anyhow::ensure!(
            expected_project_hash == project_hash,
            "expected directory {} to have project hash {expected_project_hash}, but was {project_hash}",
            local_path.display()
        );

        loaded_projects.insert(project_hash);
    }

    Ok(loaded_projects)
}

async fn insert_non_conflicting(
    brioche: &Brioche,
    directory: &mut Directory,
    path: &[u8],
    artifact: &Artifact,
) -> anyhow::Result<()> {
    let previous = directory
        .insert(brioche, path, Some(artifact.clone()))
        .await?;

    if let Some(previous) = previous {
        anyhow::ensure!(
            &previous == artifact,
            "tried to insert an artifact at {}, but a different artifact already exists at that path",
            bstr::BStr::new(path)
        );
    }

    Ok(())
}

async fn insert_merge(
    brioche: &Brioche,
    directory: &mut Directory,
    path: &[u8],
    artifact: &Artifact,
) -> anyhow::Result<()> {
    if path.is_empty() {
        let Artifact::Directory(artifact) = artifact else {
            let artifact_kind = ArtifactDiscriminants::from(artifact);
            anyhow::bail!("cannot merge root artifact with {artifact_kind:?}");
        };

        directory.merge(artifact, brioche).await?;
        return Ok(());
    }

    let previous = directory
        .insert(brioche, path, Some(artifact.clone()))
        .await?;

    match (previous, artifact) {
        (None, _) => {
            // Newly inserted, nothing to do
        }
        (Some(previous), artifact) if &previous == artifact => {
            // New value matches previous, so nothing to do
        }
        (Some(Artifact::Directory(mut previous)), Artifact::Directory(artifact)) => {
            // Merge the new directory into the previous value, then re-add it
            previous.merge(artifact, brioche).await?;
            directory
                .insert(brioche, path, Some(Artifact::Directory(previous)))
                .await?;
        }
        (Some(previous), artifact) => {
            // Both artifacts need to be directories to merge, so bail

            let previous_kind = ArtifactDiscriminants::from(&previous);
            let artifact_kind = ArtifactDiscriminants::from(artifact);
            anyhow::bail!(
                "cannot merge artifact {previous_kind:?} at path {} with {artifact_kind:?}",
                bstr::BStr::new(path)
            );
        }
    }

    Ok(())
}
