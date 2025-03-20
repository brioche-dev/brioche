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
            .project_entry(project_hash)
            .with_context(|| format!("project {project_hash} not loaded"))?;
        let project = match project {
            super::ProjectEntry::WorkspaceMember { .. } => {
                todo!();
            }
            super::ProjectEntry::Project(project) => project,
        };

        // Enqueue each dependency so they get added to the artifact
        let project_dependencies = projects.project_dependencies(project_hash)?;
        queued_projects.extend(project_dependencies.into_values());

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
    // Get a list of all project hashes included in the artifact, and validate
    // that each one has a valid name
    let mut project_hashes = HashSet::new();
    let artifact_entries = artifact.entries(brioche).await?;
    for (name, entry) in &artifact_entries {
        let name = name
            .to_str()
            .with_context(|| format!("non UTF-8 filename in artifact: {name:?}"))?;
        let project_hash: ProjectHash = name
            .parse()
            .with_context(|| format!("invalid filename in artifact: {name:?}"))?;

        anyhow::ensure!(
            matches!(entry, Artifact::Directory(_)),
            "expected artifact entry for project {project_hash} to be a directory"
        );

        project_hashes.insert(project_hash);
    }

    // Save the contents of the artifact under `projects/inner`
    let inner_dir_path = brioche.data_dir.join("projects").join("inner");
    tokio::fs::create_dir_all(&inner_dir_path).await?;
    crate::output::create_output(
        brioche,
        &Artifact::Directory(artifact.clone()),
        crate::output::OutputOptions {
            link_locals: false,
            merge: true,
            output_path: &inner_dir_path,
            resource_dir: None,
            mtime: None,
        },
    )
    .await?;

    // Create a symlink within `projects` to each project added to
    // `projects/inner`. We create a symlink within `projects` only after
    // everything from the artifact is saved so that projects are created
    // atomically (e.g. so a project isn't written to disk before its
    // dependencies).
    for project_hash in &project_hashes {
        let project_hash_string = project_hash.to_string();
        let project_symlink = brioche.data_dir.join("projects").join(&project_hash_string);
        let project_target = Path::new("inner").join(&project_hash_string);
        tokio::fs::symlink(project_target, project_symlink).await?;
    }

    // Load each project and validate that the hashes match
    for project_hash in &project_hashes {
        let project_hash_string = project_hash.to_string();
        let project_symlink = brioche.data_dir.join("projects").join(&project_hash_string);

        let loaded_project_hash = projects
            .load(
                brioche,
                &project_symlink,
                super::ProjectValidation::Standard,
                super::ProjectLocking::Locked,
            )
            .await?;
        anyhow::ensure!(
            *project_hash == loaded_project_hash,
            "saved project {project_hash} from cache, but it had unexpected hash {loaded_project_hash} after loading",
        );
    }

    Ok(project_hashes)
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
