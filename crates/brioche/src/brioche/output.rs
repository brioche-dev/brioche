use std::path::{Path, PathBuf};

use anyhow::Context as _;
use bstr::ByteSlice;

use super::{
    artifact::{CompleteArtifact, File},
    Brioche,
};

struct LocalOutputLock(());

static LOCAL_OUTPUT_MUTEX: tokio::sync::Mutex<LocalOutputLock> =
    tokio::sync::Mutex::const_new(LocalOutputLock(()));

#[derive(Debug, Clone, Copy)]
pub struct OutputOptions<'a> {
    pub output_path: &'a Path,
    pub resources_dir: Option<&'a Path>,
    pub merge: bool,
    pub link_locals: bool,
}

#[tracing::instrument(skip(brioche, artifact), fields(artifact_hash = %artifact.hash()), err)]
pub async fn create_output(
    brioche: &Brioche,
    artifact: &CompleteArtifact,
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

    create_output_inner(brioche, artifact, options, lock.as_ref()).await?;
    Ok(())
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip(brioche, artifact, link_lock), fields(artifact_hash = %artifact.hash()), err)]
async fn create_output_inner<'a: 'async_recursion>(
    brioche: &Brioche,
    artifact: &CompleteArtifact,
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
        CompleteArtifact::File(File {
            content_blob,
            executable,
            resources,
        }) => {
            if !resources.is_empty() {
                let Some(resources_dir) = options.resources_dir else {
                    anyhow::bail!("cannot output file outside of a directory, file has references");
                };

                create_output_inner(
                    brioche,
                    &CompleteArtifact::Directory(resources.clone()),
                    OutputOptions {
                        output_path: resources_dir,
                        resources_dir: Some(resources_dir),
                        merge: true,
                        link_locals: options.link_locals,
                    },
                    link_lock,
                )
                .await?;
            }

            let blob_path = super::blob::blob_path(brioche, *content_blob);
            tokio::fs::copy(&blob_path, options.output_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to copy blob from {} to {}",
                        blob_path.display(),
                        options.output_path.display()
                    )
                })?;
            set_file_permissions(options.output_path, *executable).await?;
        }
        CompleteArtifact::Symlink { target } => {
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
        CompleteArtifact::Directory(directory) => {
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

            let listing = directory.listing(brioche).await?;

            for (path, entry) in listing.entries {
                let path = bytes_to_path_component(path.as_bstr())?;
                let entry_path = options.output_path.join(path);
                let resources_dir_buf;
                let resources_dir = match options.resources_dir {
                    Some(resources_dir) => resources_dir,
                    None => {
                        resources_dir_buf = options.output_path.join("brioche-pack.d");
                        &resources_dir_buf
                    }
                };

                match (&entry.value, link_lock) {
                    (CompleteArtifact::File(file), Some(link_lock)) => {
                        if !file.resources.is_empty() {
                            create_output_inner(
                                brioche,
                                &CompleteArtifact::Directory(file.resources.clone()),
                                OutputOptions {
                                    output_path: resources_dir,
                                    resources_dir: Some(resources_dir),
                                    merge: true,
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
                                resources_dir: Some(resources_dir),
                                merge: true,
                                link_locals: options.link_locals,
                            },
                            link_lock,
                        )
                        .await?;
                    }
                }
            }

            set_directory_permissions(options.output_path).await?;
        }
    }

    Ok(())
}

pub async fn create_local_output(
    brioche: &Brioche,
    artifact: &CompleteArtifact,
) -> anyhow::Result<LocalOutput> {
    // Use a mutex to ensure we don't try to create the same local output
    // simultaneously.
    // TODO: Make this function parallelizable
    let lock = LOCAL_OUTPUT_MUTEX.lock().await;

    let result = create_local_output_inner(brioche, artifact, &lock).await?;

    Ok(result)
}

async fn create_local_output_inner(
    brioche: &Brioche,
    artifact: &CompleteArtifact,
    lock: &tokio::sync::MutexGuard<'_, LocalOutputLock>,
) -> anyhow::Result<LocalOutput> {
    let local_dir = brioche.home.join("locals");
    tokio::fs::create_dir_all(&local_dir).await?;

    let artifact_hash = artifact.hash();
    let local_path = local_dir.join(artifact_hash.to_string());
    let local_resources_dir = local_dir.join(format!("{artifact_hash}-pack.d"));

    if !tokio::fs::try_exists(&local_path).await? {
        let local_temp_dir = brioche.home.join("locals-temp");
        tokio::fs::create_dir_all(&local_temp_dir).await?;
        let temp_id = ulid::Ulid::new();
        let local_temp_path = local_temp_dir.join(temp_id.to_string());
        let local_temp_resources_dir = local_temp_dir.join(format!("{temp_id}-pack.d"));

        create_output_inner(
            brioche,
            artifact,
            OutputOptions {
                output_path: &local_temp_path,
                resources_dir: Some(&local_temp_resources_dir),
                merge: false,
                link_locals: true,
            },
            Some(lock),
        )
        .await?;

        tokio::fs::rename(&local_temp_path, &local_path)
            .await
            .context("failed to finish saving local output")?;

        if tokio::fs::try_exists(&local_temp_resources_dir).await? {
            tokio::fs::rename(&local_temp_resources_dir, &local_resources_dir)
                .await
                .context("failed to finish saving local output resources")?;
        }
    }

    let resources_dir = if tokio::fs::try_exists(&local_resources_dir).await? {
        Some(local_resources_dir)
    } else {
        None
    };

    Ok(LocalOutput {
        path: local_path,
        resources_dir,
    })
}

pub struct LocalOutput {
    pub path: PathBuf,
    pub resources_dir: Option<PathBuf>,
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

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        async fn set_file_permissions(path: &Path, executable: bool) -> anyhow::Result<()> {
            use std::os::unix::fs::PermissionsExt as _;

            let mode = if executable {
                0o755
            } else {
                0o644
            };
            let permissions = std::fs::Permissions::from_mode(mode);
            tokio::fs::set_permissions(path, permissions).await?;
            Ok(())
        }

        async fn set_directory_permissions(path: &Path) -> anyhow::Result<()> {
            use std::os::unix::fs::PermissionsExt as _;

            let permissions = std::fs::Permissions::from_mode(0o755);
            tokio::fs::set_permissions(path, permissions).await?;
            Ok(())
        }
    }
}
