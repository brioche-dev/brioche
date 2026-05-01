use std::path::Path;

use anyhow::Context as _;

pub async fn move_file(source: &Path, dest: &Path) -> anyhow::Result<MoveType> {
    let rename_result = tokio::fs::rename(source, dest).await;

    let move_type = if matches!(rename_result, Ok(())) {
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
    } else {
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

#[derive(Debug, Clone, Copy)]
pub enum MoveType {
    Rename,
    Copy,
}

#[must_use]
pub fn brioche_epoch() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_hours(262_968)
}

pub async fn set_mtime_to_brioche_epoch(path: &Path) -> anyhow::Result<()> {
    set_mtime(path, brioche_epoch()).await?;
    Ok(())
}

cfg_select! {
    unix => {
        #[must_use] pub fn is_executable(permissions: &std::fs::Permissions) -> bool {
            use std::os::unix::fs::PermissionsExt as _;

            permissions.mode() & 0o100 != 0
        }

        pub fn set_rwx(permissions: &mut std::fs::Permissions) {
            use std::os::unix::fs::PermissionsExt as _;

            let new_mode = permissions.mode() | 0o700;
            permissions.set_mode(new_mode);
        }

        pub async fn set_mtime(path: &Path, mtime: std::time::SystemTime) -> anyhow::Result<()> {
            let path = path.to_owned();
            tokio::task::spawn_blocking(move || {
                let file = std::fs::File::open(path)?;
                file.set_modified(mtime)?;
                anyhow::Ok(())
            }).await??;

            Ok(())
        }
    }
    _ => {}
}
