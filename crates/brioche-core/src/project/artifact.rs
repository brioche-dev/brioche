use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::Context as _;
use bstr::{ByteSlice as _, ByteVec as _};
use petgraph::visit::EdgeRef as _;

use crate::{
    BriocheMut, BriocheResources,
    blob::SaveBlobPermit,
    path::{AbsolutePath, RelativePath, RelativePathComponent},
    project::{
        ProjectRef, WorkspaceDefinition, WorkspaceMember,
        hash::{ContentAddressedProjectEntry, ContentAddressedWorkspacePath, WorkspaceHash},
    },
    recipe::{
        self, File, Recipe, RecipeRef, Symlink,
        build::{ArtifactBuilder, ArtifactPath, ArtifactPathComponent, insert_into_artifact},
    },
};

use super::{Projects, hash::ProjectHash};

pub async fn create_project_artifact(
    brioche: &mut BriocheMut<'_>,
    project_ref: ProjectRef,
) -> anyhow::Result<RecipeRef> {
    let mut permit = crate::blob::get_save_blob_permit().await?;

    let mut directory = Some(recipe::build::ArtifactBuilder::empty_dir());

    let project_groups =
        crate::project::hash::group_project_nodes(&brioche.state.projects, [project_ref]);

    // Compute hashes for each project
    let mut project_hashes = HashMap::new();
    let mut project_entries = HashMap::new();
    crate::project::hash::hash_projects_inner(
        brioche,
        &mut permit,
        &project_groups,
        &mut project_hashes,
        Some(&mut project_entries),
    );

    let workspace_groups = project_groups.iter().filter(|group| group.len() > 1);
    for workspace_group in workspace_groups {
        let ContentAddressedProjectEntry::WorkspaceMember {
            workspace: group_workspace_hash,
            ..
        } = &project_entries[workspace_group.iter().next().unwrap()]
        else {
            panic!("expected project entry to be a workspace member");
        };
        let workspace_path = ArtifactPath::new(format!("workspace-{group_workspace_hash}"))?;

        let mut members: Vec<_> = workspace_group
            .iter()
            .map(|project_ref| {
                let ContentAddressedProjectEntry::WorkspaceMember {
                    path,
                    workspace: workspace_hash,
                } = &project_entries[project_ref]
                else {
                    panic!("expected project entry to be a workspace member");
                };

                assert_eq!(
                    workspace_hash, group_workspace_hash,
                    "expected all project entries in group to be part of the same workspace"
                );

                path
            })
            .collect();
        members.sort();

        let members = members
            .iter()
            .map(|path| {
                let (path, name) = path
                    .parent_with_last_component()
                    .expect("todo: invalid workspace path");
                WorkspaceMember::Path(path.into(), name)
            })
            .collect();
        let workspace_definition = WorkspaceDefinition { members };
        let workspace_definition_contents = toml::to_string_pretty(&workspace_definition)
            .context("failed to serialize lockfile")?;

        let workspace_definition_blob = crate::blob::save_blob(
            brioche.resources,
            &mut permit,
            workspace_definition_contents.as_bytes(),
            crate::blob::SaveBlobOptions::default(),
        )
        .await?;
        let workspace_definition_artifact = ArtifactBuilder::File {
            content_blob: workspace_definition_blob,
            executable: false,
            resources: Box::new(None),
        };

        insert_into_artifact(
            &mut directory,
            &workspace_path.join_one(ArtifactPathComponent::entry("brioche_workspace.toml")),
            workspace_definition_artifact,
        )?;
    }

    for (project_ref, project_entry) in project_entries {
        let project_hash = project_hashes[&project_ref];
        let project_path = project_hash.to_string();

        let project_artifact = create_single_project_artifact(
            brioche.resources,
            &brioche.state.projects,
            &project_hashes,
            project_ref,
            &mut permit,
        )
        .await?;

        match project_entry {
            ContentAddressedProjectEntry::Project(_) => {
                insert_into_artifact(
                    &mut directory,
                    &ArtifactPath::new(&project_path)?,
                    project_artifact,
                )?;
            }
            ContentAddressedProjectEntry::WorkspaceMember {
                workspace: workspace_hash,
                path,
            } => {
                let workspace_path = ArtifactPath::new(format!("workspace-{workspace_hash}"))?;
                let workspace_member_path =
                    workspace_path.join(RelativePath::from(&path).try_into()?);

                insert_into_artifact(&mut directory, &workspace_member_path, project_artifact)?;

                // Add a symlink for the project into the workspace
                let project_target = format!("workspace-{workspace_hash}/{path}");
                insert_into_artifact(
                    &mut directory,
                    &ArtifactPath::new(project_hash.to_string())?,
                    ArtifactBuilder::Symlink {
                        target: project_target.into(),
                    },
                )?;
            }
        }
    }

    let directory = directory.unwrap();
    let artifact_ref = crate::recipe::build::build_artifact(brioche, &directory)?;
    Ok(artifact_ref)
}

async fn create_single_project_artifact(
    brioche: &BriocheResources,
    projects: &Projects,
    project_hashes: &HashMap<ProjectRef, ProjectHash>,
    project_ref: ProjectRef,
    permit: &mut SaveBlobPermit<'_>,
) -> anyhow::Result<ArtifactBuilder> {
    let mut artifact = Some(ArtifactBuilder::empty_dir());

    let mut files = HashMap::<ArtifactPath, AbsolutePath>::new();
    let mut directories = Vec::<(ArtifactPath, AbsolutePath)>::new();
    let mut symlinks = HashMap::<ArtifactPath, bstr::BString>::new();
    let mut globs = vec![];

    // Add each module to the artifact
    for (module_path, module_ref) in &projects.modules_by_project[&project_ref] {
        let local_project_path = &projects.local_project_paths[&project_ref];
        let module_parent_path = module_path
            .parent()
            .unwrap_or_else(|| panic!("invalid module path: {module_path}"));

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

        let path = ArtifactPath::try_from(module_path.clone())?;
        crate::recipe::build::insert_into_artifact(&mut artifact, &path, module_artifact)?;

        // Queue up any file paths referenced from statics
        for (static_query, _) in projects.module_statics(*module_ref) {
            match &static_query.query {
                super::StaticQuery::IncludeFile(include_path) => {
                    let include_path = module_parent_path.clone().join(include_path.clone());
                    let artifact_path = ArtifactPath::try_from(include_path.clone())?;
                    let include_path = local_project_path.join_subpath(include_path)?;
                    files.insert(artifact_path, include_path);
                }
                super::StaticQuery::IncludeDirectory(include_path) => {
                    let include_path = module_parent_path.clone().join(include_path.clone());
                    let artifact_path = ArtifactPath::try_from(include_path.clone())?;
                    let include_path = local_project_path.join_subpath(include_path)?;
                    directories.push((artifact_path, include_path));
                }
                super::StaticQuery::Glob { patterns } => {
                    let artifact_path = ArtifactPath::try_from(module_parent_path.clone())?;
                    let module_parent_path =
                        local_project_path.join_subpath(module_parent_path.clone())?;
                    globs.push((artifact_path, module_parent_path, patterns));
                }
                super::StaticQuery::Download { .. } | super::StaticQuery::GitRef(_) => {
                    // Nothing to add
                }
            }
        }
    }

    // Prepare the project lockfile
    let mut lockfile = projects.projects[&project_ref]
        .lockfile_state
        .lockfile()
        .clone();

    // Replace the dependencies from the lockfile. This is needed because,
    // even if the lockfile is up-to-date, the artifact version of a project
    // may need to put dependencies in the lockfile that aren't in the actual
    // lockfile (e.g. workspace members).
    let dependencies = projects.graph.edges(project_ref.0).filter_map(|edge| {
        let crate::project::ProjectEdge::ProjectDependency(dep_name) = edge.weight() else {
            return None;
        };
        let dep_ref = ProjectRef(edge.target());
        let dep_hash = project_hashes[&dep_ref];

        Some((dep_name.clone(), dep_hash))
    });
    lockfile.dependencies.clear();
    lockfile.dependencies.extend(dependencies);

    // Add the lockfile to the artifact
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
    let lockfile_path = ArtifactPath::try_from(lockfile_path)?;
    crate::recipe::build::insert_into_artifact(&mut artifact, &lockfile_path, lockfile_artifact)?;

    // Resolve static glob patterns into files/directories/symlinks to add
    for (artifact_path, path, patterns) in globs {
        tracing::info!(
            path = artifact_path.display_pretty(),
            ?patterns,
            "adding globs"
        );

        let mut glob_set = globset::GlobSetBuilder::new();
        for pattern in patterns {
            let glob = globset::GlobBuilder::new(pattern)
                .case_insensitive(false)
                .literal_separator(true)
                .backslash_escape(true)
                .empty_alternates(true)
                .build()?;
            glob_set.add(glob);
        }
        let glob_set = glob_set.build()?;

        (files, directories, symlinks) = tokio::task::spawn_blocking(move || {
            let system_path = path.to_system_path()?;
            for entry in walkdir::WalkDir::new(&system_path) {
                let entry = entry?;

                let entry_path = crate::path::canonicalize_system_path_sync(entry.path())?;
                let relative_entry_path = crate::path::relative_path_between(&path, &entry_path)
                    .with_context(|| {
                        format!(
                            "failed to resolve matched path {entry_path} relative to module path {path}",
                        )
                    })?;

                let relative_entry_system_path = relative_entry_path.to_system_path()?;
                if !glob_set.is_match(&relative_entry_system_path) {
                    tracing::debug!(path = %relative_entry_system_path.display(), "path does not match");
                    continue;
                }

                let mut artifact_subpath = artifact_path.clone();
                for component in relative_entry_path.components() {
                    let RelativePathComponent::Normal(component) = component else {
                        panic!("invalid path between module path {path} and matched path {entry_path}");
                    };
                    artifact_subpath.components.push(ArtifactPathComponent::DirectoryEntry(component.clone()));
                }

                let file_type = entry.file_type();
                if file_type.is_file() {
                    tracing::debug!(path = %relative_entry_path, "matched file");
                    let entry_path = crate::path::canonicalize_system_path_sync(entry.path())?;
                    files.insert(artifact_subpath, entry_path);
                } else if file_type.is_dir() {
                    tracing::debug!(path = %relative_entry_path, "matched dir");
                    let entry_path = crate::path::canonicalize_system_path_sync(entry.path())?;
                    directories.push((artifact_subpath, entry_path));
                } else if file_type.is_symlink() {
                    tracing::debug!(path = %relative_entry_path, "matched symlink");
                    let target_path = std::fs::read_link(entry.path())
                        .context("failed to read symlink target")?;
                    let target = <Vec<u8>>::from_path_buf(target_path.clone()).map_err(|_| {
                        anyhow::anyhow!("invalid symlink target at {}", entry.path().display())
                    })?;
                    symlinks.insert(artifact_subpath, bstr::BString::new(target));
                } else {
                    anyhow::bail!("unknown file type at {}", entry.path().display());
                }
            }
            anyhow::Ok((files, directories, symlinks))
        })
        .await??;
    }

    // Add directories from statics (recursively), and queue up files/symlinks
    // along the way
    let mut visited_directories = HashSet::new();
    while let Some((artifact_path, path)) = directories.pop() {
        if !visited_directories.insert(artifact_path.clone()) {
            break;
        }

        crate::recipe::build::insert_into_artifact(
            &mut artifact,
            &artifact_path,
            ArtifactBuilder::empty_dir(),
        )?;

        let system_path = path.to_system_path()?;
        let mut entries = tokio::fs::read_dir(&system_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let filename = entry.file_name();
            let filename = <[u8]>::from_os_str(&filename)
                .with_context(|| format!("invalid filename: {}", filename.display()))?;
            let filename = bstr::BStr::new(filename);
            let artifact_subpath = artifact_path
                .clone()
                .join_one(ArtifactPathComponent::entry(filename));

            let file_type = entry.file_type().await?;
            if file_type.is_file() {
                let entry_path = path.join_one(filename);
                files.insert(artifact_subpath, entry_path);
            } else if file_type.is_dir() {
                let entry_path = path.join_one(filename);
                directories.push((artifact_subpath, entry_path));
            } else if file_type.is_symlink() {
                let target_path =
                    std::fs::read_link(entry.path()).context("failed to read symlink target")?;
                let target = <Vec<u8>>::from_path_buf(target_path.clone()).map_err(|_| {
                    anyhow::anyhow!("invalid symlink target at {}", entry.path().display())
                })?;
                symlinks.insert(artifact_subpath, bstr::BString::new(target));
            } else {
                anyhow::bail!("unknown file type at {}", entry.path().display());
            }
        }
    }

    // Add files from statics
    let mut buffer = vec![];
    for (artifact_path, path) in files {
        let system_path = path.to_system_path()?;
        let file_blob = crate::blob::save_blob_from_file(
            brioche,
            permit,
            &system_path,
            crate::blob::SaveBlobOptions::default(),
            &mut buffer,
        )
        .await?;
        let file_metadata = tokio::fs::metadata(&system_path).await?;
        let executable = crate::fs_utils::is_executable(&file_metadata.permissions());

        let file_artifact = ArtifactBuilder::File {
            content_blob: file_blob,
            executable,
            resources: Box::new(None),
        };

        crate::recipe::build::insert_into_artifact(&mut artifact, &artifact_path, file_artifact)?;
    }

    // Add symlinks from statics
    for (artifact_path, target) in symlinks {
        crate::recipe::build::insert_into_artifact(
            &mut artifact,
            &artifact_path,
            ArtifactBuilder::Symlink { target },
        )?;
    }

    let artifact = artifact.unwrap();
    Ok(artifact)
}

pub async fn save_projects_from_artifact(
    brioche: &BriocheMut<'_>,
    artifact_ref: RecipeRef,
) -> anyhow::Result<HashMap<ProjectHash, crate::path::AbsolutePath>> {
    let mut project_hashes = HashSet::new();
    let mut needed_workspace_hashes = HashSet::new();
    let mut included_workspace_hashes = HashSet::new();

    let artifact = brioche.state.recipes.get_recipe(artifact_ref);
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
            let entry = brioche.state.recipes.get_recipe(*entry_ref);
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

            let entry = brioche.state.recipes.get_recipe(*entry_ref);
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
                    let member_path: ContentAddressedWorkspacePath = member_path.parse()?;
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

    let inner_dir_path = brioche.resources.data_dir.join("projects").join("inner");
    let project_temp_dir_path = brioche.resources.data_dir.join("projects-temp");

    tokio::fs::create_dir_all(&inner_dir_path).await?;
    tokio::fs::create_dir_all(&project_temp_dir_path).await?;

    // Save the contents of the artifact under `projects/inner`
    write_artifact_atomic(
        brioche.resources,
        &brioche.state.recipes,
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
        let project_symlink = brioche
            .resources
            .data_dir
            .join("projects")
            .join(&project_hash_string);
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
    brioche: &BriocheResources,
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
