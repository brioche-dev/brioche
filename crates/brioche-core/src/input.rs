use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use bstr::{ByteSlice as _, ByteVec as _};

use crate::fs_utils::{is_executable, set_directory_rwx_recursive};

use super::{
    recipe::{Artifact, Directory, File, Meta, WithMeta},
    Brioche,
};

#[derive(Debug, Clone, Copy)]
pub struct InputOptions<'a> {
    pub input_path: &'a Path,
    pub remove_input: bool,
    pub resource_dir: Option<&'a Path>,
    pub input_resource_dirs: &'a [PathBuf],
    pub meta: &'a Arc<Meta>,
}

impl InputOptions<'_> {
    fn has_resource_dirs(&self) -> bool {
        self.resource_dir.is_some() || !self.input_resource_dirs.is_empty()
    }
}

#[tracing::instrument(skip(brioche), err)]
pub async fn create_input(
    brioche: &Brioche,
    options: InputOptions<'_>,
) -> anyhow::Result<WithMeta<Artifact>> {
    // Ensure directories that we will remove are writable and executable
    if options.remove_input {
        set_directory_rwx_recursive(options.input_path).await?;
        if let Some(resource_dir) = options.resource_dir {
            set_directory_rwx_recursive(resource_dir).await?;
        }
    }

    let result = create_input_inner(brioche, options).await?;
    Ok(result)
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip(brioche), err)]
pub async fn create_input_inner(
    brioche: &Brioche,
    options: InputOptions<'async_recursion>,
) -> anyhow::Result<WithMeta<Artifact>> {
    let metadata = tokio::fs::symlink_metadata(options.input_path)
        .await
        .with_context(|| {
            format!(
                "failed to get metadata for {}",
                options.input_path.display()
            )
        })?;

    if metadata.is_file() {
        let resources = if options.has_resource_dirs() {
            let pack = tokio::task::spawn_blocking({
                let input_path = options.input_path.to_owned();
                move || {
                    let input_file = std::fs::File::open(&input_path)
                        .with_context(|| format!("failed to open file {}", input_path.display()))?;
                    let extracted = brioche_pack::extract_pack(input_file).ok();
                    let pack = extracted.map(|extracted| extracted.pack);
                    anyhow::Ok(pack)
                }
            })
            .await?
            .context("failed to extract resource pack")?;

            let pack_paths = pack.iter().flat_map(|pack| pack.paths());

            let mut pack_paths: Vec<_> = pack_paths.collect();
            let mut resources = Directory::default();
            while let Some(pack_path) = pack_paths.pop() {
                let path = pack_path.to_path().context("invalid resource path")?;
                let resource =
                    find_resource(options.resource_dir, options.input_resource_dirs, path).await?;

                let Some(resource) = resource else {
                    anyhow::bail!(
                        "resource for input {} not found: {}",
                        options.input_path.display(),
                        path.display()
                    );
                };

                let resource_artifact = create_input_inner(
                    brioche,
                    InputOptions {
                        input_path: &resource.path,
                        remove_input: false,
                        resource_dir: options.resource_dir,
                        input_resource_dirs: options.input_resource_dirs,
                        meta: options.meta,
                    },
                )
                .await?;

                tracing::debug!(resource = %resource.path.display(), "found resource");
                resources
                    .insert(brioche, &pack_path, Some(resource_artifact))
                    .await?;

                // Add the symlink's target to the resource dir as well
                if resource.metadata.is_symlink() {
                    let target = match tokio::fs::canonicalize(&resource.path).await {
                        Ok(target) => target,
                        Err(err) => {
                            tracing::warn!(resource = %resource.path.display(), "invalid resource symlink: {err}");
                            continue;
                        }
                    };
                    let canonical_resource_dir =
                        tokio::fs::canonicalize(&resource.resource_dir).await;
                    let canonical_resource_dir = match canonical_resource_dir {
                        Ok(target) => target,
                        Err(err) => {
                            tracing::warn!(resource_dir = %resource.resource_dir.display(), "failed to canonicalize resource dir: {err}");
                            continue;
                        }
                    };
                    let target = match target.strip_prefix(&canonical_resource_dir) {
                        Ok(target) => target,
                        Err(err) => {
                            tracing::warn!(resource = %resource.path.display(), "resource symlink target not under resource dir: {err}");
                            continue;
                        }
                    };

                    tracing::debug!(target = %target.display(), "queueing symlink resource target");

                    let target = Vec::<u8>::from_path_buf(target.to_owned()).map_err(|_| {
                        anyhow::anyhow!("invalid symlink target at {}", resource.path.display())
                    })?;

                    pack_paths.push(target.into());
                } else if resource.metadata.is_dir() {
                    let mut dir = tokio::fs::read_dir(&resource.path).await.with_context(|| {
                        format!("failed to read directory {}", resource.path.display())
                    })?;

                    tracing::debug!(resource_path = %resource.path.display(), "queueing directory entry resource");

                    while let Some(entry) = dir.next_entry().await.transpose() {
                        let entry = entry.context("failed to read directory entry")?;
                        let entry_path = path.join(entry.file_name());
                        let entry_path = <Vec<u8> as bstr::ByteVec>::from_path_buf(entry_path)
                            .map_err(|_| {
                                anyhow::anyhow!(
                                    "invalid entry {} in directory {}",
                                    entry.file_name().to_string_lossy(),
                                    resource.path.display()
                                )
                            })?;

                        pack_paths.push(entry_path.into());
                    }
                }
            }

            resources
        } else {
            Directory::default()
        };

        let blob_hash = {
            let permit = super::blob::get_save_blob_permit().await?;
            super::blob::save_blob_from_file(
                brioche,
                permit,
                options.input_path,
                super::blob::SaveBlobOptions::default().remove_input(options.remove_input),
            )
            .await
        }?;
        let permissions = metadata.permissions();
        let executable = is_executable(&permissions);

        Ok(WithMeta::new(
            Artifact::File(File {
                content_blob: blob_hash,
                executable,
                resources,
            }),
            options.meta.clone(),
        ))
    } else if metadata.is_dir() {
        let mut dir = tokio::fs::read_dir(options.input_path)
            .await
            .with_context(|| {
                format!("failed to read directory {}", options.input_path.display())
            })?;

        let mut result_dir_entries = BTreeMap::new();

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

            let result_entry = create_input_inner(
                brioche,
                InputOptions {
                    input_path: &entry.path(),
                    ..options
                },
            )
            .await?;

            result_dir_entries.insert(entry_name, result_entry);
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

        let result_dir = Directory::create(brioche, &result_dir_entries).await?;
        Ok(WithMeta::new(
            Artifact::Directory(result_dir),
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
            Artifact::Symlink { target },
            options.meta.clone(),
        ))
    } else {
        anyhow::bail!(
            "unsupported input file type at {}",
            options.input_path.display()
        );
    }
}

pub struct FoundResource<'a> {
    pub path: PathBuf,
    pub metadata: std::fs::Metadata,
    pub resource_dir: &'a Path,
}

async fn find_resource<'a>(
    resource_dir: Option<&'a Path>,
    input_resource_dirs: &'a [PathBuf],
    subpath: &Path,
) -> anyhow::Result<Option<FoundResource<'a>>> {
    let resource_dirs = resource_dir
        .into_iter()
        .chain(input_resource_dirs.iter().map(|dir| &**dir));

    for resource_dir in resource_dirs {
        let resource_path = resource_dir.join(subpath);
        let resource_metadata = tokio::fs::symlink_metadata(&resource_path).await;
        match resource_metadata {
            Ok(metadata) => {
                return Ok(Some(FoundResource {
                    path: resource_path,
                    metadata,
                    resource_dir,
                }));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).context("failed to get metadata for resource");
            }
        }
    }

    Ok(None)
}
