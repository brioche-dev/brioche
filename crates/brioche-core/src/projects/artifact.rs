use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::Arc,
};

use anyhow::Context as _;
use bstr::ByteSlice as _;

use crate::{
    Brioche,
    blob::SaveBlobPermit,
    path::RelativePath,
    projects::{
        ProjectRef,
        hash::{ContentAddressedProjectEntry, WorkspaceHash},
    },
    recipe::{
        Artifact, ArtifactKind, Directory, File, Recipe, RecipeRef, Symlink, build::ArtifactBuilder,
    },
};

use super::{Project, Projects, Workspace, hash::ProjectHash};

pub async fn create_project_artifact(
    brioche: &Brioche,
    project_ref: ProjectRef,
) -> anyhow::Result<RecipeRef> {
    let projects = brioche.projects.read().await;
    let mut recipes = brioche.recipes.write().await;
    let mut permit = crate::blob::get_save_blob_permit().await?;

    let mut directory = Directory::default();

    // Create a copy of the graph, but keeping only project nodes that
    // are reachable from the target project
    let mut graph = projects.graph.clone();
    let mut dfs_space = petgraph::algo::DfsSpace::default();
    graph.retain_nodes(|graph, index| match &graph[index] {
        crate::projects::ProjectNode::Project => {
            petgraph::algo::has_path_connecting(&*graph, project_ref.0, index, Some(&mut dfs_space))
        }
        crate::projects::ProjectNode::Workspace | crate::projects::ProjectNode::Module => false,
    });

    // Group nodes by finding the strongly-connected components of the graph.
    // This effectively finds cyclic projects in the graph that we should
    // group together, and puts acyclic projects into a group of one element.
    // The result is additionally topographically sorted, so every project
    // naturally comes after all of its dependencies
    let node_groups = petgraph::algo::tarjan_scc(&graph);

    // Compute hashes for each project
    let mut project_hashes = HashMap::new();
    crate::projects::hash::hash_projects_inner(&projects, &node_groups, &mut project_hashes);

    for group_nodes in node_groups {
        let group_nodes: HashSet<_> = group_nodes.into_iter().collect();

        if group_nodes.len() > 1 {
            unimplemented!("cyclic project");
        }

        let project_ref = group_nodes.iter().next().unwrap();
        let project_ref = ProjectRef(*project_ref);

        let project_hash = project_hashes[&project_ref];
        let project_path = project_hash.to_string();

        let project_artifact = create_single_project_artifact(
            brioche,
            &mut recipes,
            &projects,
            project_ref,
            &mut permit,
        )
        .await?;
        directory
            .entries
            .insert(bstr::BString::from(project_path), project_artifact);
    }

    let artifact_ref = recipes.insert_recipe(Arc::new(Recipe::Directory(directory)));
    Ok(artifact_ref)
}

async fn create_single_project_artifact(
    brioche: &Brioche,
    recipes: &mut crate::recipe::Recipes,
    projects: &Projects,
    project_ref: ProjectRef,
    permit: &mut SaveBlobPermit<'_>,
) -> anyhow::Result<RecipeRef> {
    let mut artifact = Some(ArtifactBuilder::empty_dir());

    // TODO: Add statics

    // Add each module to the artifact
    for (module_path, module_ref) in &projects.modules_by_project[&project_ref] {
        let module = &projects.modules[module_ref];
        let source = module
            .source
            .as_ref()
            .map_err(|error| anyhow::anyhow!("error loading module: {error}"))?;
        let content_blob = crate::blob::save_blob(
            brioche,
            permit,
            source.as_bytes(),
            crate::blob::SaveBlobOptions::default(),
        )
        .await?;

        let module_artifact = ArtifactBuilder::File {
            content_blob,
            executable: false,
            resources: Box::new(None),
        };

        let path = crate::recipe::build::ArtifactPath::try_from(module_path.clone())?;
        crate::recipe::build::insert_into_artifact(
            &mut artifact,
            &path,
            &path.components,
            module_artifact,
        )?;
    }

    // Add the lockfile to the artifact
    let lockfile = &projects.projects[&project_ref].lockfile;
    let lockfile_contents =
        serde_json::to_string_pretty(&lockfile).context("failed to serialize lockfile")?;

    let lockfile_blob = crate::blob::save_blob(
        brioche,
        permit,
        lockfile_contents.as_bytes(),
        crate::blob::SaveBlobOptions::default(),
    )
    .await?;
    let lockfile_artifact = ArtifactBuilder::File {
        content_blob: lockfile_blob,
        executable: false,
        resources: Box::new(None),
    };

    let lockfile_path = RelativePath::new("brioche.lock");
    let lockfile_path = crate::recipe::build::ArtifactPath::try_from(lockfile_path)?;
    crate::recipe::build::insert_into_artifact(
        &mut artifact,
        &lockfile_path,
        &lockfile_path.components,
        lockfile_artifact,
    )?;

    let artifact = artifact.unwrap();
    let artifact = crate::recipe::build::build_artifact(&artifact, recipes)?;
    Ok(artifact)
}

pub async fn save_projects_from_artifact(
    brioche: &Brioche,
    artifact_ref: RecipeRef,
) -> anyhow::Result<HashMap<ProjectHash, crate::path::AbsolutePath>> {
    let mut recipes = brioche.recipes.write().await;

    let mut project_hashes = HashSet::new();
    let mut needed_workspace_hashes = HashSet::new();
    let mut included_workspace_hashes = HashSet::new();

    let artifact = recipes.get_recipe(artifact_ref);
    let Recipe::Directory(artifact) = &**artifact else {
        anyhow::bail!("expected Directory, but got {:?}", artifact.kind());
    };

    // Validate that the projects and workspaces in the project look valid
    // based on their filenames and file types
    for (name, entry_ref) in &artifact.entries {
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
            let entry = recipes.get_recipe(*entry_ref);
            anyhow::ensure!(
                matches!(**entry, Recipe::Directory(_)),
                "expected artifact entry for workspace {workspace_hash} to be a directory"
            );
        } else {
            // Artifact entry should be a project

            // Parse the project hash from the entry name
            let project_hash: ProjectHash = name
                .parse()
                .with_context(|| format!("invalid filename in artifact: {name:?}"))?;
            project_hashes.insert(project_hash);

            let entry = recipes.get_recipe(*entry_ref);
            match &**entry {
                Recipe::Directory(_) => {
                    // Normal project (not part of a workspace)
                }
                Recipe::Symlink(Symlink { target }) => {
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
                    let member_path = RelativePath::new(member_path);
                    anyhow::ensure!(
                        member_path.is_normalized_subpath(),
                        "invlaid workspace member symlink for project {project_hash} in artifact"
                    );
                    needed_workspace_hashes.insert(workspace_hash);

                    // Validate that the project hash matches using the
                    // workspace hash and the member path
                    let project_entry = ContentAddressedProjectEntry::WorkspaceMember {
                        workspace: workspace_hash,
                        path: member_path,
                    };
                    let project_entry_hash = project_entry.project_hash();
                    anyhow::ensure!(
                        project_hash == project_entry_hash,
                        "project hash {project_hash} did not match hash for workspace member for symlink {target}"
                    );
                }
                recipe => {
                    anyhow::bail!(
                        "unexpected recipe for project {project_hash}: {:?}",
                        recipe.kind()
                    );
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
        &recipes,
        artifact_ref,
        &inner_dir_path,
        &project_temp_dir_path,
    )
    .await?;

    // Create a symlink within `projects` to each project added to
    // `projects/inner`. We create a symlink within `projects` only after
    // everything from the artifact is saved so that projects are created
    // atomically (e.g. so a project isn't written to disk before its
    // dependencies).
    let mut project_paths = HashMap::new();
    for project_hash in &project_hashes {
        let project_hash_string = project_hash.to_string();
        let project_symlink = brioche.data_dir.join("projects").join(&project_hash_string);
        let project_target = Path::new("inner").join(&project_hash_string);

        let result = tokio::fs::symlink(project_target, &project_symlink).await;
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

        let project_path = crate::path::canonicalize_system_path(&project_symlink).await?;
        project_paths.insert(*project_hash, project_path);
    }

    Ok(project_paths)
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
    recipes: &crate::recipe::Recipes,
    artifact_ref: RecipeRef,
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

    let artifact = recipes.get_recipe(artifact_ref);
    match &**artifact {
        Recipe::File(File {
            content_blob,
            executable,
            resources,
        }) => {
            anyhow::ensure!(
                resources.is_none(),
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
            let blob_path = crate::blob::local_blob_path(brioche, *content_blob);
            tokio::fs::copy(blob_path, &temp_path).await?;
            set_file_permissions(
                &temp_path,
                SetFilePermissions {
                    executable: *executable,
                    readonly: false,
                },
            )
            .await?;

            // Rename the file to its final path
            tokio::fs::rename(&temp_path, output_path).await?;
        }
        Recipe::Symlink(Symlink { target }) => {
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
        Recipe::Directory(directory) => {
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
            for (name, entry_ref) in &directory.entries {
                let name = name
                    .to_str()
                    .with_context(|| format!("invalid filename {name:?} in artifact"))?;
                let entry_path = output_path.join(name);
                Box::pin(write_artifact_atomic(
                    brioche,
                    recipes,
                    *entry_ref,
                    &entry_path,
                    temp_dir,
                ))
                .await?;
            }
        }
        recipe => {
            anyhow::bail!("expected an artifact, got {:?}", recipe.kind());
        }
    }

    Ok(())
}

struct SetFilePermissions {
    executable: bool,
    readonly: bool,
}

cfg_select! {
    unix => {
        async fn set_file_permissions(path: &Path, permissions: SetFilePermissions) -> anyhow::Result<()> {
            use std::os::unix::fs::PermissionsExt as _;

            let mode = match permissions {
                SetFilePermissions { executable: true, readonly: false } => 0o755,
                SetFilePermissions { executable: false, readonly: false } => 0o644,
                SetFilePermissions { executable: true, readonly: true } => 0o555,
                SetFilePermissions { executable: false, readonly: true } => 0o444,
            };
            let permissions = std::fs::Permissions::from_mode(mode);
            tokio::fs::set_permissions(path, permissions).await?;
            Ok(())
        }
    }
    _ => {}
}
