use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use bstr::{ByteSlice as _, ByteVec as _};

use crate::{
    Brioche,
    blob::SaveBlobOptions,
    recipe::build::{ArtifactBuilder, ArtifactPath, ArtifactPathComponent},
};

pub fn load_artifact_sync(
    brioche: &Brioche,
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
                let entry_artifact_subpath = artifact_subpath
                    .clone()
                    .child(ArtifactPathComponent::DirectoryEntry(filename));
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
                path: path.clone(),
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

#[derive(Debug, thiserror::Error)]
enum LoadArtifactError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

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
