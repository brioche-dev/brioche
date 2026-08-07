use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
};

use bstr::{ByteSlice as _, ByteVec as _};

use crate::{
    BriocheResources,
    blob::SaveBlobOptions,
    path::RelativePathComponent,
    recipe::build::{ArtifactBuilder, ArtifactPath, ArtifactPathComponent},
};

pub async fn load_artifact(
    brioche: Arc<BriocheResources>,
    path: PathBuf,
    artifact_subpath: ArtifactPath,
) -> Result<ArtifactBuilder, LoadArtifactError> {
    let mut permit = crate::blob::get_save_blob_permit()
        .await
        .expect("todo: failed to get save_blob_permit");
    tokio::task::spawn_blocking(move || {
        let mut artifact = None;
        load_artifact_sync(
            &brioche,
            &mut permit,
            &mut artifact,
            &path,
            artifact_subpath,
        )?;
        let artifact = artifact.unwrap();
        Ok(artifact)
    })
    .await
    .unwrap()
}

pub async fn load_artifact_glob(
    brioche: Arc<BriocheResources>,
    path: PathBuf,
    artifact_subpath: ArtifactPath,
    patterns: Vec<String>,
) -> Result<ArtifactBuilder, LoadArtifactError> {
    let mut permit = crate::blob::get_save_blob_permit()
        .await
        .expect("todo: failed to get save_blob_permit");
    tokio::task::spawn_blocking(move || {
        let mut artifact = None;
        load_artifact_glob_sync(
            &brioche,
            &mut permit,
            &mut artifact,
            &path,
            &artifact_subpath,
            &patterns,
        )?;
        let artifact = artifact.unwrap();
        Ok(artifact)
    })
    .await
    .unwrap()
}

pub fn load_artifact_sync(
    brioche: &BriocheResources,
    save_blob_permit: &mut crate::blob::SaveBlobPermit<'_>,
    container: &mut Option<ArtifactBuilder>,
    path: &Path,
    artifact_subpath: ArtifactPath,
) -> Result<(), LoadArtifactError> {
    let mut buffer = vec![];
    let mut queue = VecDeque::from_iter([(path.to_owned(), artifact_subpath)]);
    while let Some((path, artifact_subpath)) = queue.pop_front() {
        let metadata = std::fs::symlink_metadata(&path)?;

        let artifact = if metadata.is_file() {
            let mut file = std::fs::File::open(&path)?;
            let content_blob = crate::blob::save_blob_from_reader_sync(
                brioche,
                save_blob_permit,
                &mut file,
                SaveBlobOptions::default(),
                &mut buffer,
            )
            .map_err(|error| LoadArtifactError::SaveBlobError {
                error_message: error.to_string(),
            })?;

            let permissions = metadata.permissions();
            let executable = crate::fs_utils::is_executable(&permissions);

            ArtifactBuilder::File {
                executable,
                content_blob,
                resources: Box::new(None),
            }
        } else if metadata.is_dir() {
            let entries = std::fs::read_dir(&path)?;
            for entry in entries {
                let entry = entry?;
                let filename = entry.file_name();
                let filename = <[u8]>::from_os_str(&filename).ok_or_else(|| {
                    LoadArtifactError::InvalidOsFilename {
                        filename: filename.clone(),
                    }
                })?;
                let filename = bstr::BString::from(filename);
                let entry_artifact_subpath =
                    artifact_subpath.join_one(ArtifactPathComponent::DirectoryEntry(filename));
                queue.push_back((entry.path(), entry_artifact_subpath));
            }

            ArtifactBuilder::empty_dir()
        } else if metadata.is_symlink() {
            let target_path = std::fs::read_link(&path)?;
            let target = <Vec<u8>>::from_path_buf(target_path.clone()).map_err(|_| {
                LoadArtifactError::InvalidOsFilename {
                    filename: target_path.as_os_str().to_owned(),
                }
            })?;
            let target = bstr::BString::new(target);

            ArtifactBuilder::Symlink { target }
        } else {
            return Err(LoadArtifactError::UnsupportedFileType {
                path,
                file_type: metadata.file_type(),
            });
        };

        crate::recipe::build::insert_or_replace_in_artifact(container, &artifact_subpath, artifact)
            .map_err(|error| LoadArtifactError::InsertInArtifactError {
                error_message: error.to_string(),
            })?;
    }

    Ok(())
}

pub fn load_artifact_glob_sync(
    brioche: &BriocheResources,
    save_blob_permit: &mut crate::blob::SaveBlobPermit<'_>,
    container: &mut Option<ArtifactBuilder>,
    path: &Path,
    artifact_subpath: &ArtifactPath,
    patterns: &[String],
) -> Result<(), LoadArtifactError> {
    let absolute_path = crate::path::canonicalize_system_path_sync(path)?;

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

    for entry in walkdir::WalkDir::new(path) {
        let entry = entry?;
        let entry_path = crate::path::canonicalize_system_path_sync(entry.path())?;
        let relative_entry_path = crate::path::relative_path_between(&absolute_path, &entry_path)?;

        let relative_entry_system_path = relative_entry_path.to_system_path()?;
        if !glob_set.is_match(&relative_entry_system_path) {
            continue;
        }

        let mut artifact_subpath = artifact_subpath.clone();
        for component in relative_entry_path.components() {
            let RelativePathComponent::Normal(component) = component else {
                panic!(
                    "invalid path between module path {absolute_path} and matched path {entry_path}"
                );
            };
            artifact_subpath
                .components
                .push(ArtifactPathComponent::DirectoryEntry(component.clone()));
        }

        load_artifact_sync(
            brioche,
            save_blob_permit,
            container,
            entry.path(),
            artifact_subpath,
        )?;
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum LoadArtifactError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    GlobsetError(#[from] globset::Error),

    #[error(transparent)]
    WalkdirError(#[from] walkdir::Error),

    #[error(transparent)]
    CanonicalSystemPathError(#[from] crate::path::CanonicalSystemPathError),

    #[error(transparent)]
    RelativePathBetweenError(#[from] crate::path::RelativePathBetweenError),

    #[error(transparent)]
    ToSystemPathError(#[from] crate::path::ToSystemPathError),

    #[error("error saving blob: {error_message}")]
    SaveBlobError { error_message: String },

    #[error("error inserting in artifact: {error_message}")]
    InsertInArtifactError { error_message: String },

    #[error("invalid OS filename: {}", filename.display())]
    InvalidOsFilename { filename: std::ffi::OsString },

    #[error("unsupported file type at {}: {file_type:?}", path.display())]
    UnsupportedFileType {
        path: PathBuf,
        file_type: std::fs::FileType,
    },
}
