use std::path::Path;

use anyhow::Context as _;

pub async fn move_file(source: &Path, dest: &Path) -> anyhow::Result<MoveType> {
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
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(err).context("failed to ensure file was removed after renaming");
                }
            }

            MoveType::Rename
        }
        Err(_) => {
            let metadata = tokio::fs::symlink_metadata(source).await?;
            if metadata.is_dir() {
                anyhow::bail!("cannot move directory across filesystems");
            } else if metadata.is_file() || metadata.is_symlink() {
                atomic_copy(source, dest).await?;
                tokio::fs::remove_file(source).await?;
                MoveType::Copy
            } else {
                anyhow::bail!("cannot move unsupported file type across filesystems");
            }
        }
    };

    Ok(move_type)
}

pub async fn atomic_copy(source: &Path, dest: &Path) -> anyhow::Result<()> {
    let dest_temp = dest.with_extension(format!("tmp-{}", ulid::Ulid::new()));
    tokio::fs::copy(source, &dest_temp)
        .await
        .context("failed to copy file to temp")?;
    tokio::fs::rename(dest_temp, dest)
        .await
        .context("failed to rename temp file")?;
    Ok(())
}

pub async fn try_remove(path: &Path) -> anyhow::Result<bool> {
    let meta = tokio::fs::symlink_metadata(path).await;
    let meta = match meta {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };

    if meta.is_dir() {
        tokio::fs::remove_dir_all(path).await?;
    } else {
        tokio::fs::remove_file(path).await?;
    }

    Ok(true)
}

#[derive(Debug, Clone, Copy)]
pub enum MoveType {
    Rename,
    Copy,
}
