use std::{
    collections::{HashSet, VecDeque},
    path::Path,
};

use anyhow::Context as _;
use bstr::ByteSlice as _;
use relative_path::RelativePathBuf;

use crate::{
    Brioche,
    project::{ProjectEntry, WorkspaceHash},
    recipe::{Artifact, ArtifactDiscriminants, Directory},
};

use super::{Project, ProjectHash, Projects, Workspace, analyze::StaticQuery};

pub async fn create_artifact_with_projects(
    brioche: &Brioche,
    projects: &Projects,
    project_hashes: &[ProjectHash],
) -> anyhow::Result<Directory> {
    let mut artifact = Directory::default();

    let mut written_projects = HashSet::new();
    let mut written_workspaces = HashSet::new();
    let mut queued_projects = VecDeque::from_iter(project_hashes.iter().copied());
    let mut permit = crate::blob::get_save_blob_permit().await?;

    while let Some(project_hash) = queued_projects.pop_front() {
        // Ensure we only add each project once
        if !written_projects.insert(project_hash) {
            continue;
        }

        // Enqueue each dependency so they get added to the artifact
        let project_dependencies = projects.project_dependencies(project_hash)?;
        queued_projects.extend(project_dependencies.into_values());

        // Get the project entry
        let project = projects
            .project_entry(project_hash)
            .with_context(|| format!("project {project_hash} not loaded"))?;
        match project {
            super::ProjectEntry::Project(project) => {
                // A normal (non-cyclic) project. Build an artifact for it
                // and add it to the root of the artifact

                let project_artifact = project_artifact(brioche, &project, &mut permit).await?;

                let project_path = project_hash.to_string();
                insert_non_conflicting(
                    brioche,
                    &mut artifact,
                    project_path.as_bytes(),
                    &Artifact::Directory(project_artifact),
                )
                .await?;
            }
            super::ProjectEntry::WorkspaceMember {
                workspace: workspace_hash,
                path,
            } => {
                // A project that's part of a (cyclic) workspace
                let workspace_path = format!("workspace-{workspace_hash}");
                let project_path = project_hash.to_string();

                // Add the workspace to the artifact if it hasn't
                // already been added
                if !written_workspaces.insert(workspace_hash) {
                    let workspace = projects.workspace(workspace_hash)?;
                    let workspace_artifact =
                        workspace_artifact(brioche, &workspace, &mut permit).await?;
                    insert_non_conflicting(
                        brioche,
                        &mut artifact,
                        workspace_path.as_bytes(),
                        &Artifact::Directory(workspace_artifact),
                    )
                    .await?;
                }

                // Add a symlink for the project into the workspace
                let project_target = format!("{workspace_path}/{path}");
                insert_non_conflicting(
                    brioche,
                    &mut artifact,
                    project_path.as_bytes(),
                    &Artifact::Symlink {
                        target: project_target.into(),
                    },
                )
                .await?;
            }
        }
    }

    Ok(artifact)
}

pub async fn save_projects_from_artifact(
    brioche: &Brioche,
    projects: &Projects,
    artifact: &Directory,
) -> anyhow::Result<HashSet<ProjectHash>> {
    let mut project_hashes = HashSet::new();
    let mut needed_workspace_hashes = HashSet::new();
    let mut included_workspace_hashes = HashSet::new();

    // Validate that the projects and workspaces in the project look valid
    // based on their filenames and file types
    let artifact_entries = artifact.entries(brioche).await?;
    for (name, entry) in &artifact_entries {
        let name = name
            .to_str()
            .with_context(|| format!("non UTF-8 filename in artifact: {name:?}"))?;

        if let Some(workspace_hash) = name.strip_prefix("workspace-") {
            // Artifact entry looks like a workspace ("workspace-{hash}")

            // Parse the workspace hash from the filename
            let workspace_hash: WorkspaceHash = workspace_hash
                .parse()
                .with_context(|| format!("invalid filename in artifact: {name:?}"))?;
            included_workspace_hashes.insert(workspace_hash);

            // Validate that the workspace is stored as a directory
            anyhow::ensure!(
                matches!(entry, Artifact::Directory(_)),
                "expected artifact entry for workspace {workspace_hash} to be a directory"
            );
        } else {
            // Artifact entry should be a project

            // Parse the project hash from the entry name
            let project_hash: ProjectHash = name
                .parse()
                .with_context(|| format!("invalid filename in artifact: {name:?}"))?;
            project_hashes.insert(project_hash);

            match entry {
                Artifact::Directory(_) => {
                    // Normal project (not part of a workspace)
                }
                Artifact::Symlink { target } => {
                    // Project is a symlink, which indicates its a member
                    // of a workspace

                    let target = target.to_str().with_context(|| {
                        format!("non UTF-8 symlink target in artifact: {target:?}")
                    })?;

                    // Parse the workspace hash and member path from
                    // the symlink target
                    let (workspace_path, member_path) = target.split_once('/').with_context(|| format!("invalid workspace member symlink for project {project_hash} in artifact"))?;
                    let workspace_hash = workspace_path.strip_prefix("workspace-").with_context(|| format!("invalid workspace member symlink for project {project_hash} in artifact"))?;
                    let workspace_hash: WorkspaceHash = workspace_hash.parse().with_context(|| format!("invalid workspace member symlink for project {project_hash} in artifact"))?;
                    let member_path = RelativePathBuf::from(member_path);
                    anyhow::ensure!(
                        member_path.is_normalized(),
                        "invlaid workspace member symlink for project {project_hash} in artifact"
                    );
                    needed_workspace_hashes.insert(workspace_hash);

                    // Validate that the project hash matches using the
                    // workspace hash and the member path
                    let project_entry = ProjectEntry::WorkspaceMember {
                        workspace: workspace_hash,
                        path: member_path,
                    };
                    let project_entry_hash = ProjectHash::from_serializable(&project_entry)?;
                    anyhow::ensure!(
                        project_hash == project_entry_hash,
                        "project hash {project_hash} did not match hash for workspace member for symlink {target}"
                    );
                }
                Artifact::File(_) => {
                    anyhow::bail!("did not expect project {project_hash} to be a file");
                }
            }
        }
    }

    // Validate that there are no missing or extra workspaces
    let missing_workspaces: HashSet<_> = needed_workspace_hashes
        .difference(&included_workspace_hashes)
        .collect();
    let extra_workspaces: HashSet<_> = included_workspace_hashes
        .difference(&needed_workspace_hashes)
        .collect();
    anyhow::ensure!(
        missing_workspaces.is_empty() && extra_workspaces.is_empty(),
        "the set of workspaces in project artifact does not match the set of workspaces needed by the projects from the artifact (missing: {missing_workspaces:?}, extra: {extra_workspaces:?})"
    );

    let inner_dir_path = brioche.data_dir.join("projects").join("inner");
    let project_temp_dir_path = brioche.data_dir.join("projects-temp");

    tokio::fs::create_dir_all(&inner_dir_path).await?;
    tokio::fs::create_dir_all(&project_temp_dir_path).await?;

    // Save the contents of the artifact under `projects/inner`
    write_artifact_atomic(
        brioche,
        &Artifact::Directory(artifact.clone()),
        &inner_dir_path,
        &project_temp_dir_path,
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

        let result = tokio::fs::symlink(project_target, project_symlink).await;
        match result {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Tried to create symlink, but the path already exists. Since
                // this symlink is only written once all the project's
                // dependencies are written, the existing path should already
                // be valid, so we can safely ignore this error
            }
            Err(error) => {
                return Err(error.into());
            }
        }
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

async fn project_artifact(
    brioche: &Brioche,
    project: &Project,
    permit: &mut crate::blob::SaveBlobPermit<'_>,
) -> anyhow::Result<Directory> {
    let mut project_artifact = Directory::default();

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
                    let recipe_hash = recipe_hash.context("no recipe hash for include static")?;
                    let recipe = crate::recipe::get_recipe(brioche, recipe_hash).await?;
                    let artifact: crate::recipe::Artifact = recipe.try_into().map_err(|_| {
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
                    let artifact: crate::recipe::Artifact = recipe.try_into().map_err(|_| {
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
    let lockfile = super::project_lockfile(project);
    let lockfile_contents =
        serde_json::to_string_pretty(&lockfile).context("failed to serialize lockfile")?;

    let lockfile_blob = crate::blob::save_blob(
        brioche,
        permit,
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

    Ok(project_artifact)
}
async fn workspace_artifact(
    brioche: &Brioche,
    workspace: &Workspace,
    permit: &mut crate::blob::SaveBlobPermit<'_>,
) -> anyhow::Result<Directory> {
    let mut workspace_artifact = Directory::default();

    // Add each workspace member to the artifact
    for (path, member) in &workspace.members {
        let member_artifact = project_artifact(brioche, member, permit).await?;
        insert_non_conflicting(
            brioche,
            &mut workspace_artifact,
            path.as_str().as_bytes(),
            &Artifact::Directory(member_artifact),
        )
        .await?;
    }
    // Add the workspace definition to the artifact
    let workspace_definition =
        super::workspace_definition(workspace).context("failed to create workspace definition")?;
    let workspace_definition_contents =
        toml::to_string_pretty(&workspace_definition).context("failed to serialize lockfile")?;

    let workspace_definition_blob = crate::blob::save_blob(
        brioche,
        permit,
        workspace_definition_contents.as_bytes(),
        crate::blob::SaveBlobOptions::default(),
    )
    .await?;
    let workspace_definition_artifact = Artifact::File(crate::recipe::File {
        content_blob: workspace_definition_blob,
        executable: false,
        resources: Directory::default(),
    });

    insert_non_conflicting(
        brioche,
        &mut workspace_artifact,
        b"brioche_workspace.toml",
        &workspace_definition_artifact,
    )
    .await?;

    Ok(workspace_artifact)
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

/// Write an artifact to the provided path, ensuring each file is created
/// atomically. Files and symlinks are written to a temporary path then renamed,
/// and are skipped if the destination path exists before writing. Directories
/// are merged.
///
/// This is very similar to [`brioche_core::outputs::create_output`], but is
/// designed specifically for the use-case of writing project artifacts, where
/// each top-level entry uses a content-addressed name.
async fn write_artifact_atomic(
    brioche: &Brioche,
    artifact: &Artifact,
    output_path: &Path,
    temp_dir: &Path,
) -> anyhow::Result<()> {
    let metadata = tokio::fs::symlink_metadata(&output_path).await;
    let metadata = match metadata {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to get metadata for path {}", output_path.display())
            });
        }
    };

    match artifact {
        Artifact::File(file) => {
            anyhow::ensure!(
                file.resources.is_empty(),
                "cannot write artifact with file resources",
            );

            if let Some(metadata) = metadata {
                // Path already exists, so validate that it's a file
                // and return early

                anyhow::ensure!(
                    metadata.is_file(),
                    "trying to write file for artifact, but non-file exists at {}",
                    output_path.display()
                );
                return Ok(());
            }

            // Write the file to a temp path
            let temp_path = temp_dir.join(format!("temp-file-{}", ulid::Ulid::new()));
            crate::output::create_output(
                brioche,
                artifact,
                crate::output::OutputOptions {
                    link_locals: false,
                    merge: false,
                    output_path: &temp_path,
                    resource_dir: None,
                    mtime: None,
                },
            )
            .await?;

            // Rename the file to its final path
            tokio::fs::rename(&temp_path, output_path).await?;
        }
        Artifact::Symlink { target } => {
            if let Some(metadata) = metadata {
                // Path already exists, so validate that it's a symlink
                // and return early

                anyhow::ensure!(
                    metadata.is_symlink(),
                    "trying to write symlink for artifact, but non-symlink exists at {}",
                    output_path.display()
                );
                return Ok(());
            }

            // Create the symlink
            let target = target.to_path()?;
            let result = tokio::fs::symlink(&target, output_path).await;

            match result {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Symlink was created since we got the path
                    // metadata, so treat this as a success
                    return Ok(());
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to create symlink {} -> {}",
                            output_path.display(),
                            target.display(),
                        )
                    })?;
                }
            }
        }
        Artifact::Directory(directory) => {
            if let Some(metadata) = metadata {
                // Path already exists, so validate that it's a directory

                anyhow::ensure!(
                    metadata.is_dir(),
                    "trying to write directory for artifact, but non-directory exists at {}",
                    output_path.display()
                );
            } else {
                // Directory doesn't exist, so create it

                let result = tokio::fs::create_dir(output_path).await;
                match result {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // Directory was created since we got the path
                        // metadata, so treat this as a success
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("failed to create directory {}", output_path.display())
                        })?;
                    }
                }
            }

            // Write each entry artifact
            let entries = directory.entries(brioche).await?;
            for (name, entry) in entries {
                let name = name
                    .to_str()
                    .with_context(|| format!("invalid filename {name:?} in artifact"))?;
                let entry_path = output_path.join(name);
                Box::pin(write_artifact_atomic(
                    brioche,
                    &entry,
                    &entry_path,
                    temp_dir,
                ))
                .await?;
            }
        }
    }

    Ok(())
}
