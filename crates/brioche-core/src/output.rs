use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use bstr::ByteSlice;

use super::{
    recipe::{Artifact, Directory, File},
    Brioche,
};

struct LocalOutputLock(());

static LOCAL_OUTPUT_MUTEX: tokio::sync::Mutex<LocalOutputLock> =
    tokio::sync::Mutex::const_new(LocalOutputLock(()));

#[derive(Debug, Clone, Copy)]
pub struct OutputOptions<'a> {
    pub output_path: &'a Path,
    pub resource_dir: Option<&'a Path>,
    pub merge: bool,
    pub mtime: Option<std::time::SystemTime>,
    pub link_locals: bool,
}

#[tracing::instrument(skip(brioche, artifact), fields(artifact_hash = %artifact.hash()), err)]
pub async fn create_output(
    brioche: &Brioche,
    artifact: &Artifact,
    options: OutputOptions<'_>,
) -> anyhow::Result<()> {
    let lock = if options.link_locals {
        // If we use links into the `~/.local/share/brioche/locals` directory,
        // lock a mutex to ensure we don't write to the same local more
        // than once at a time
        Some(LOCAL_OUTPUT_MUTEX.lock().await)
    } else {
        None
    };

    // Fetch all blobs before creating the output
    fetch_descendent_artifact_blobs(brioche, artifact).await?;

    // Create the output
    create_output_inner(brioche, artifact, options, lock.as_ref()).await?;
    Ok(())
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip(brioche, artifact, link_lock), fields(artifact_hash = %artifact.hash()), err)]
async fn create_output_inner<'a: 'async_recursion>(
    brioche: &Brioche,
    artifact: &Artifact,
    options: OutputOptions<'a>,
    link_lock: Option<&'a tokio::sync::MutexGuard<'a, LocalOutputLock>>,
) -> anyhow::Result<()> {
    let link_lock = match (options.link_locals, link_lock) {
        (false, _) => None,
        (true, Some(lock)) => Some(lock),
        (true, None) => {
            anyhow::bail!(
                "tried to call `create_output_inner` with `link_locals`, but no lock was provided"
            );
        }
    };

    match artifact {
        Artifact::File(File {
            content_blob,
            executable,
            resources,
        }) => {
            if resources.is_empty() {
                let blob_path = super::blob::local_blob_path(brioche, *content_blob);

                anyhow::ensure!(
                    tokio::fs::try_exists(&blob_path).await?,
                    "blob not found: {}",
                    blob_path.display(),
                );

                if options.link_locals && !*executable {
                    crate::fs_utils::try_remove(options.output_path).await?;
                    tokio::fs::hard_link(&blob_path, options.output_path)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to create hardlink from {} to {}",
                                blob_path.display(),
                                options.output_path.display()
                            )
                        })?;
                } else {
                    tokio::fs::copy(&blob_path, options.output_path)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to copy blob from {} to {}",
                                blob_path.display(),
                                options.output_path.display()
                            )
                        })?;

                    // Set the file permissions and mtime. We set the file
                    // to be read-only and reset the file's modified time
                    // if `link_locals` is enabled even if the file wasn't
                    // created as a hardlink. That way, the permissions are
                    // the same whether or not we used a hardlink.
                    set_file_permissions(
                        options.output_path,
                        SetFilePermissions {
                            executable: *executable,
                            readonly: options.link_locals,
                        },
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to set output file permissions of {}",
                            options.output_path.display()
                        )
                    })?;

                    if let Some(mtime) = options.mtime {
                        crate::fs_utils::set_mtime(options.output_path, mtime)
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to set output file modified time of {}",
                                    options.output_path.display()
                                )
                            })?;
                    } else if options.link_locals {
                        crate::fs_utils::set_mtime_to_brioche_epoch(options.output_path)
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to set output file modified time of {}",
                                    options.output_path.display()
                                )
                            })?;
                    }
                }
            } else {
                let Some(resource_dir) = options.resource_dir else {
                    anyhow::bail!("cannot output file outside of a directory, file has resources");
                };

                create_output_inner(
                    brioche,
                    &Artifact::Directory(resources.clone()),
                    OutputOptions {
                        output_path: resource_dir,
                        resource_dir: Some(resource_dir),
                        merge: true,
                        mtime: None,
                        link_locals: options.link_locals,
                    },
                    link_lock,
                )
                .await?;

                let artifact_without_resources = Artifact::File(File {
                    content_blob: *content_blob,
                    executable: *executable,
                    resources: Directory::default(),
                });

                if let Some(link_lock) = link_lock {
                    let local_path =
                        create_local_output_inner(brioche, &artifact_without_resources, link_lock)
                            .await?;
                    crate::fs_utils::try_remove(options.output_path).await?;
                    tokio::fs::hard_link(&local_path.path, options.output_path)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to create hardlink from {} to {}",
                                local_path.path.display(),
                                options.output_path.display()
                            )
                        })?;
                } else {
                    create_output_inner(
                        brioche,
                        &artifact_without_resources,
                        OutputOptions {
                            output_path: options.output_path,
                            resource_dir: None,
                            merge: options.merge,
                            mtime: options.mtime,
                            link_locals: options.link_locals,
                        },
                        link_lock,
                    )
                    .await?;
                }
            }
        }
        Artifact::Symlink { target } => {
            let target = target.to_path()?;
            if options.merge {
                // Try to remove the file if it already exists so we can
                // replace it. In practice, we should end up replacing it
                // with an identical symlink even if it does exist
                if tokio::fs::remove_file(options.output_path).await.is_ok() {
                    tracing::debug!(target = %target.display(), "removed conflicting file to create symlink");
                }
            }
            tokio::fs::symlink(&target, options.output_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to create symlink {} -> {}",
                        options.output_path.display(),
                        target.display(),
                    )
                })?;
        }
        Artifact::Directory(directory) => {
            let result = tokio::fs::create_dir(options.output_path).await;

            match result {
                Ok(()) => {}
                Err(error)
                    if options.merge && error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to create directory {}",
                            options.output_path.display()
                        )
                    })?;
                }
            }

            for (path, entry) in directory.entries(brioche).await? {
                let path = bytes_to_path_component(path.as_bstr())?;
                let entry_path = options.output_path.join(path);
                let resource_dir_buf;
                let resource_dir = match options.resource_dir {
                    Some(resource_dir) => resource_dir,
                    None => {
                        resource_dir_buf = options.output_path.join("brioche-resources.d");
                        &resource_dir_buf
                    }
                };

                match (&entry.value, link_lock) {
                    (Artifact::File(file), Some(link_lock)) => {
                        if !file.resources.is_empty() {
                            create_output_inner(
                                brioche,
                                &Artifact::Directory(file.resources.clone()),
                                OutputOptions {
                                    output_path: resource_dir,
                                    resource_dir: Some(resource_dir),
                                    merge: true,
                                    mtime: options.mtime,
                                    link_locals: options.link_locals,
                                },
                                Some(link_lock),
                            )
                            .await?;
                        }

                        // If `link_locals` is enabled, create a local output
                        // for the file, then hardlink to it

                        let local_output =
                            create_local_output_inner(brioche, &entry.value, link_lock).await?;
                        crate::fs_utils::try_remove(&entry_path).await?;
                        tokio::fs::hard_link(&local_output.path, &entry_path)
                            .await
                            .context("failed to create hardlink into Brioche `locals` directory")?;
                    }
                    _ => {
                        create_output_inner(
                            brioche,
                            &entry.value,
                            OutputOptions {
                                output_path: &entry_path,
                                resource_dir: Some(resource_dir),
                                merge: true,
                                mtime: options.mtime,
                                link_locals: options.link_locals,
                            },
                            link_lock,
                        )
                        .await?;
                    }
                }
            }

            set_directory_permissions(
                options.output_path,
                SetDirectoryPermissions { readonly: false },
            )
            .await?;
        }
    }

    Ok(())
}

pub async fn create_local_output(
    brioche: &Brioche,
    artifact: &Artifact,
) -> anyhow::Result<LocalOutput> {
    // Use a mutex to ensure we don't try to create the same local output
    // simultaneously.
    // TODO: Make this function parallelizable
    let lock = LOCAL_OUTPUT_MUTEX.lock().await;

    // Fetch all blobs before creating the output
    fetch_descendent_artifact_blobs(brioche, artifact).await?;

    // Create the output
    let result = create_local_output_inner(brioche, artifact, &lock).await?;

    Ok(result)
}

async fn create_local_output_inner(
    brioche: &Brioche,
    artifact: &Artifact,
    lock: &tokio::sync::MutexGuard<'_, LocalOutputLock>,
) -> anyhow::Result<LocalOutput> {
    let local_dir = brioche.home.join("locals");
    tokio::fs::create_dir_all(&local_dir).await?;

    let artifact_hash = artifact.hash();
    let local_path = local_dir.join(artifact_hash.to_string());
    let local_resource_dir = local_dir.join(format!("{artifact_hash}-resources.d"));

    if !try_exists_and_ensure_local_meta(&local_path).await? {
        let local_temp_dir = brioche.home.join("locals-temp");
        tokio::fs::create_dir_all(&local_temp_dir).await?;
        let temp_id = ulid::Ulid::new();
        let local_temp_path = local_temp_dir.join(temp_id.to_string());
        let local_temp_resource_dir = local_temp_dir.join(format!("{temp_id}-resources.d"));

        create_output_inner(
            brioche,
            artifact,
            OutputOptions {
                output_path: &local_temp_path,
                resource_dir: Some(&local_temp_resource_dir),
                merge: false,
                mtime: None,
                link_locals: true,
            },
            Some(lock),
        )
        .await?;

        tokio::fs::rename(&local_temp_path, &local_path)
            .await
            .context("failed to finish saving local output")?;

        match artifact {
            Artifact::File(file) => {
                set_file_permissions(
                    &local_path,
                    SetFilePermissions {
                        executable: file.executable,
                        readonly: true,
                    },
                )
                .await
                .context("failed to set permissions for local file")?;
                crate::fs_utils::set_mtime_to_brioche_epoch(&local_path)
                    .await
                    .context("failed to set modified time for local file")?;
            }
            Artifact::Directory(_) => {
                set_directory_permissions(&local_path, SetDirectoryPermissions { readonly: true })
                    .await
                    .context("failed to set permissions for local output directory")?
            }
            Artifact::Symlink { .. } => {}
        }

        if tokio::fs::try_exists(&local_temp_resource_dir).await? {
            tokio::fs::rename(&local_temp_resource_dir, &local_resource_dir)
                .await
                .context("failed to finish saving local output resources")?;

            set_directory_permissions(
                &local_resource_dir,
                SetDirectoryPermissions { readonly: true },
            )
            .await
            .context("failed to set permissions for local output resources")?;
        }
    }

    let resource_dir = if try_exists_and_ensure_local_meta(&local_resource_dir).await? {
        Some(local_resource_dir)
    } else {
        None
    };

    Ok(LocalOutput {
        path: local_path,
        resource_dir,
    })
}

pub struct LocalOutput {
    pub path: PathBuf,
    pub resource_dir: Option<PathBuf>,
}

async fn fetch_descendent_artifact_blobs(
    brioche: &Brioche,
    artifact: &Artifact,
) -> anyhow::Result<()> {
    // Find artifact blobs
    let mut blobs = HashSet::new();
    crate::references::descendent_artifact_blobs(brioche, [artifact.clone()], &mut blobs).await?;

    // Fetch all referenced blobs
    crate::registry::fetch_blobs(brioche.clone(), &blobs).await?;

    Ok(())
}

/// Check if a path exists, and change the file metadata to ensure it matches
/// other local values (i.e. files are marked as read-only and the file
/// modified time is set to the Unix epoch). Returns `true` if the path
/// exists and is now read-only, `false` if the path does not exist, or `Err(_)`
/// if an error occurred.
async fn try_exists_and_ensure_local_meta(path: &Path) -> anyhow::Result<bool> {
    let metadata = tokio::fs::symlink_metadata(path).await;

    let metadata = match metadata {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };

    let mut permissions = metadata.permissions();
    if !permissions.readonly() {
        permissions.set_readonly(true);
        tokio::fs::set_permissions(path, permissions)
            .await
            .with_context(|| format!("failed to mark path as read-only: {}", path.display()))?;
    }

    let mtime = metadata
        .modified()
        .context("failed to get file modified time")?;
    let is_mtime_at_epoch = mtime
        .duration_since(crate::fs_utils::brioche_epoch())
        .is_ok_and(|duration| duration.is_zero());

    if metadata.is_file() && !is_mtime_at_epoch {
        crate::fs_utils::set_mtime_to_brioche_epoch(path).await?;
    }

    Ok(true)
}

fn bytes_to_path_component(bytes: &bstr::BStr) -> anyhow::Result<std::path::PathBuf> {
    use bstr::ByteSlice as _;

    let os_str = bytes.to_os_str()?;
    let path_buf = std::path::PathBuf::from(os_str);
    let mut components = path_buf.components();
    let Some(first_component) = components.next() else {
        anyhow::bail!("empty filename: {:?}", String::from_utf8_lossy(bytes));
    };

    anyhow::ensure!(
        components.next().is_none(),
        "illegal filename: {:?}",
        String::from_utf8_lossy(bytes)
    );
    anyhow::ensure!(
        matches!(first_component, std::path::Component::Normal(_)),
        "illegal filename: {:?}",
        String::from_utf8_lossy(bytes)
    );

    Ok(path_buf)
}

struct SetFilePermissions {
    executable: bool,
    readonly: bool,
}

struct SetDirectoryPermissions {
    readonly: bool,
}

cfg_if::cfg_if! {
    if #[cfg(unix)] {
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

        async fn set_directory_permissions(path: &Path, permissions: SetDirectoryPermissions) -> anyhow::Result<()> {
            use std::os::unix::fs::PermissionsExt as _;

            let mode = match permissions {
                SetDirectoryPermissions { readonly: false } => 0o755,
                SetDirectoryPermissions { readonly: true } => 0o555,
            };

            let permissions = std::fs::Permissions::from_mode(mode);
            tokio::fs::set_permissions(path, permissions).await?;
            Ok(())
        }
    }
}
