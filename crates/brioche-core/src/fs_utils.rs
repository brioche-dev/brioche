use std::path::{Path, PathBuf};

pub async fn move_file(source: &Path, dest: &Path) -> Result<MoveType, AtomicIoError> {
    let rename_result = tokio::fs::rename(source, dest).await;

    let move_type = match rename_result {
        Ok(()) => {
            // On Linux, the rename(2) syscall (used by tokio::fs::rename at
            // the time of writing) is not guaranteed to be atomic, meaning
            // that `source` can still appear to exist after the rename
            // finishes (this isn't theoretical either, this is something
            // that we have seen in practice). To account for this, we
            // explicitly remove the source file after renaming to ensure the
            // file no longer exists at the source path.
            let remove_result = tokio::fs::remove_file(source).await;
            match remove_result {
                Ok(()) => {
                    tracing::debug!(source = %source.display(), dest = %dest.display(), "removed file after renaming");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(AtomicIoError::FailedToRemoveAfterRenaming {
                        error,
                        path: source.to_path_buf(),
                    });
                }
            }

            MoveType::Rename
        }
        Err(error) => {
            let metadata = tokio::fs::symlink_metadata(source).await?;
            if metadata.is_dir() {
                return Err(AtomicIoError::FailedToMoveDirectory {
                    error,
                    from: source.to_path_buf(),
                    to: dest.to_path_buf(),
                });
            } else if metadata.is_file() || metadata.is_symlink() {
                atomic_copy(source, dest).await?;
                tokio::fs::remove_file(source).await?;
                MoveType::Copy
            } else {
                return Err(AtomicIoError::FailedToMoveUnsupportedFileType {
                    error,
                    from: source.to_path_buf(),
                    to: dest.to_path_buf(),
                });
            }
        }
    };

    Ok(move_type)
}

#[derive(Debug, Clone, Copy)]
pub enum MoveType {
    Rename,
    Copy,
}

pub async fn atomic_copy(source: &Path, dest: &Path) -> Result<(), AtomicIoError> {
    let dest_temp = dest.with_extension(format!("tmp-{}", ulid::Ulid::new()));
    tokio::fs::copy(source, &dest_temp).await.map_err(|error| {
        AtomicIoError::CopySourceToTempError {
            error,
            from: source.to_path_buf(),
            temp: dest_temp.clone(),
        }
    })?;
    tokio::fs::rename(&dest_temp, dest).await.map_err(|error| {
        AtomicIoError::MoveTempToDestinationError {
            error,
            temp: dest_temp,
            to: dest.to_path_buf(),
        }
    })?;
    Ok(())
}

#[must_use]
pub fn brioche_epoch() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_hours(262_968)
}

pub async fn set_mtime_to_brioche_epoch(path: &Path) -> std::io::Result<()> {
    set_mtime(path, brioche_epoch()).await?;
    Ok(())
}

cfg_select! {
    unix => {
        #[must_use] pub fn is_executable(permissions: &std::fs::Permissions) -> bool {
            use std::os::unix::fs::PermissionsExt as _;

            permissions.mode() & 0o100 != 0
        }

        pub async fn set_mtime(path: &Path, mtime: std::time::SystemTime) -> std::io::Result<()> {
            let path = path.to_owned();
            tokio::task::spawn_blocking(move || {
                let file = std::fs::File::open(path)?;
                file.set_modified(mtime)?;
                std::io::Result::Ok(())
            }).await.unwrap()?;

            Ok(())
        }
    }
    _ => {}
}

#[derive(Debug, thiserror::Error)]
pub enum AtomicIoError {
    #[error("failed to remove file '{}' after renaming: {error}", .path.display())]
    FailedToRemoveAfterRenaming {
        #[source]
        error: std::io::Error,
        path: PathBuf,
    },

    #[error("failed to move directory from '{}' to '{}': {error}", .from.display(), .to.display())]
    FailedToMoveDirectory {
        #[source]
        error: std::io::Error,
        from: PathBuf,
        to: PathBuf,
    },

    #[error("failed to move unsupported file type from '{}' to '{}': {error}", .from.display(), .to.display())]
    FailedToMoveUnsupportedFileType {
        #[source]
        error: std::io::Error,
        from: PathBuf,
        to: PathBuf,
    },

    #[error("error copying file '{}' to temp path '{}'", .from.display(), .temp.display())]
    CopySourceToTempError {
        #[source]
        error: std::io::Error,
        from: PathBuf,
        temp: PathBuf,
    },

    #[error("error moving temp file '{}' to destination path '{}'", .temp.display(), .to.display())]
    MoveTempToDestinationError {
        #[source]
        error: std::io::Error,
        temp: PathBuf,
        to: PathBuf,
    },

    #[error(transparent)]
    IoError(#[from] std::io::Error),
}
