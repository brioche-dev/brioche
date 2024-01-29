use std::{path::Path, sync::Arc};

use anyhow::Context as _;
use bstr::{ByteSlice as _, ByteVec as _};

use super::{
    artifact::{CompleteArtifact, Directory, DirectoryListing, File, Meta, WithMeta},
    Brioche,
};

#[derive(Debug, Clone, Copy)]
pub struct InputOptions<'a> {
    pub input_path: &'a Path,
    pub remove_input: bool,
    pub resources_dir: Option<&'a Path>,
    pub meta: &'a Arc<Meta>,
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip(brioche), err)]
pub async fn create_input(
    brioche: &Brioche,
    options: InputOptions<'async_recursion>,
) -> anyhow::Result<WithMeta<CompleteArtifact>> {
    let metadata = tokio::fs::symlink_metadata(options.input_path)
        .await
        .with_context(|| {
            format!(
                "failed to get metadata for {}",
                options.input_path.display()
            )
        })?;

    if metadata.is_file() {
        let resources = match options.resources_dir {
            Some(resources_dir) => {
                let pack = tokio::task::spawn_blocking({
                    let input_path = options.input_path.to_owned();
                    move || {
                        let input_file = std::fs::File::open(&input_path).with_context(|| {
                            format!("failed to open file {}", input_path.display())
                        })?;
                        let pack = brioche_pack::extract_pack(input_file).ok();
                        anyhow::Ok(pack)
                    }
                })
                .await?
                .context("failed to extract resource pack")?;

                let pack_paths = pack.iter().flat_map(|pack| pack.paths());

                let mut pack_paths: Vec<_> = pack_paths.collect();
                let mut resources = DirectoryListing::default();
                while let Some(pack_path) = pack_paths.pop() {
                    let path = pack_path.to_path().context("invalid resource path")?;
                    let resource_path = resources_dir.join(path);
                    let resource_metadata = tokio::fs::symlink_metadata(&resource_path).await;
                    let resource_metadata = match resource_metadata {
                        Ok(metadata) => Some(metadata),
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                        Err(err) => return Err(err).context("failed to get metadata for resource"),
                    };
                    let resource_metadata = resource_metadata.as_ref();

                    if let Some(resource_metadata) = resource_metadata {
                        let resource = create_input(
                            brioche,
                            InputOptions {
                                input_path: &resource_path,
                                remove_input: false,
                                resources_dir: Some(resources_dir),
                                meta: options.meta,
                            },
                        )
                        .await?;

                        tracing::debug!(resource = %resource_path.display(), "found resource");
                        resources
                            .insert(brioche, &pack_path, Some(resource))
                            .await?;

                        // Add the symlink's target to the resources dir as well
                        if resource_metadata.is_symlink() {
                            let target = match tokio::fs::canonicalize(&resource_path).await {
                                Ok(target) => target,
                                Err(err) => {
                                    tracing::warn!(resource = %resource_path.display(), "invalid resource symlink: {err}");
                                    continue;
                                }
                            };
                            let target = match target.strip_prefix(resources_dir) {
                                Ok(target) => target,
                                Err(err) => {
                                    tracing::warn!(resource = %resource_path.display(), "resource symlink target not under resources dir: {err}");
                                    continue;
                                }
                            };

                            tracing::debug!(target = %target.display(), "queueing symlink resource target");

                            let target =
                                Vec::<u8>::from_path_buf(target.to_owned()).map_err(|_| {
                                    anyhow::anyhow!(
                                        "invalid symlink target at {}",
                                        resource_path.display()
                                    )
                                })?;

                            pack_paths.push(target.into());
                        } else if resource_metadata.is_dir() {
                            let mut dir =
                                tokio::fs::read_dir(&resource_path).await.with_context(|| {
                                    format!("failed to read directory {}", resource_path.display())
                                })?;

                            tracing::debug!(resource_path = %resource_path.display(), "queueing directory entry resource");

                            while let Some(entry) = dir.next_entry().await.transpose() {
                                let entry = entry.context("failed to read directory entry")?;
                                let entry_path = path.join(entry.file_name());
                                let entry_path = <Vec<u8> as bstr::ByteVec>::from_path_buf(
                                    entry_path,
                                )
                                .map_err(|_| {
                                    anyhow::anyhow!(
                                        "invalid entry {} in directory {}",
                                        entry.file_name().to_string_lossy(),
                                        resource_path.display()
                                    )
                                })?;

                                pack_paths.push(entry_path.into());
                            }
                        }
                    } else {
                        tracing::warn!("missing resource {}", resource_path.display());
                    }
                }

                resources
            }
            None => DirectoryListing::default(),
        };
        let resources = Directory::create(brioche, &resources).await?;

        let blob_id = super::blob::save_blob_from_file(
            brioche,
            options.input_path,
            super::blob::SaveBlobOptions::default().remove_input(options.remove_input),
        )
        .await?;
        let permissions = metadata.permissions();
        let executable = is_executable(&permissions).await;

        Ok(WithMeta::new(
            CompleteArtifact::File(File {
                content_blob: blob_id,
                executable,
                resources: Box::new(CompleteArtifact::Directory(resources)),
            }),
            options.meta.clone(),
        ))
    } else if metadata.is_dir() {
        let mut dir = tokio::fs::read_dir(options.input_path)
            .await
            .with_context(|| {
                format!("failed to read directory {}", options.input_path.display())
            })?;

        let mut result_dir = DirectoryListing::default();

        while let Some(entry) = dir.next_entry().await? {
            let entry_name = <Vec<u8> as bstr::ByteVec>::from_os_string(entry.file_name())
                .map_err(|_| {
                    anyhow::anyhow!(
                        "invalid file name {} in directory {}",
                        entry.file_name().to_string_lossy(),
                        options.input_path.display()
                    )
                })?;
            let entry_name = bstr::BString::from(entry_name);

            let result_entry = create_input(
                brioche,
                InputOptions {
                    input_path: &entry.path(),
                    ..options
                },
            )
            .await?;

            result_dir.entries.insert(entry_name, result_entry);
        }

        if options.remove_input {
            tokio::fs::remove_dir(options.input_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to remove directory at {}",
                        options.input_path.display()
                    )
                })?;
        }

        let result_dir = Directory::create(brioche, &result_dir).await?;
        Ok(WithMeta::new(
            CompleteArtifact::Directory(result_dir),
            options.meta.clone(),
        ))
    } else if metadata.is_symlink() {
        let target = tokio::fs::read_link(options.input_path)
            .await
            .with_context(|| {
                format!("failed to read symlink at {}", options.input_path.display())
            })?;
        let target = <Vec<u8> as bstr::ByteVec>::from_path_buf(target).map_err(|_| {
            anyhow::anyhow!("invalid symlink target at {}", options.input_path.display())
        })?;
        let target = bstr::BString::from(target);

        if options.remove_input {
            tokio::fs::remove_file(options.input_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to remove symlink at {}",
                        options.input_path.display()
                    )
                })?;
        }

        Ok(WithMeta::new(
            CompleteArtifact::Symlink { target },
            options.meta.clone(),
        ))
    } else {
        anyhow::bail!(
            "unsupported input file type at {}",
            options.input_path.display()
        );
    }
}

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        async fn is_executable(permissions: &std::fs::Permissions) -> bool {
            use std::os::unix::fs::PermissionsExt as _;

            permissions.mode() & 0o100 != 0
        }
    }
}
