use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use relative_path::RelativePath;

pub fn logical_path(path: &Path) -> PathBuf {
    let mut components = vec![];
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                components.push(component);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop();
            }
        }
    }

    PathBuf::from_iter(components)
}

pub fn is_subpath(path: &RelativePath) -> bool {
    let mut depth: i32 = 0;
    for component in path.components() {
        match component {
            relative_path::Component::CurDir => {}
            relative_path::Component::ParentDir => {
                depth = depth.checked_sub(1).unwrap();
            }
            relative_path::Component::Normal(_) => {
                depth = depth.checked_add(1).unwrap();
            }
        }

        if depth < 0 {
            return false;
        }
    }

    true
}

pub async fn is_file(path: &Path) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };

    metadata.is_file()
}

pub async fn is_dir(path: &Path) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };

    metadata.is_dir()
}

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

#[async_recursion::async_recursion]
pub async fn set_directory_rwx_recursive(path: &Path) -> anyhow::Result<()> {
    let metadata = tokio::fs::symlink_metadata(path).await;
    let metadata = match metadata {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to get metadata for directory {}", path.display())
            });
        }
    };

    if metadata.is_dir() {
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            set_rwx(&mut permissions);
            tokio::fs::set_permissions(path, permissions)
                .await
                .with_context(|| {
                    format!(
                        "failed to set write permissions for directory {}",
                        path.display()
                    )
                })?;
        }

        let mut dir = tokio::fs::read_dir(path)
            .await
            .with_context(|| format!("failed to read directory {}", path.display()))?;
        while let Some(entry) = dir.next_entry().await? {
            set_directory_rwx_recursive(&entry.path()).await?;
        }
    }

    Ok(())
}

/// A timestamp used for as the modified time for files during Brioche builds.
/// Defined as 2000-01-01 00:00:00 UTC, or 946,684,800 seconds after the Unix
/// epoch.
///
/// The main motivation for not using the Unix epoch is that ZIP files don't
/// support dates eariler than 1980, which is inconvenient. The chosen timestamp
/// beyond that is arbitrary.
pub fn brioche_epoch() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800)
}

pub async fn set_mtime_to_brioche_epoch(path: &Path) -> anyhow::Result<()> {
    set_mtime(path, brioche_epoch()).await?;
    Ok(())
}

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        pub fn is_executable(permissions: &std::fs::Permissions) -> bool {
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
}

#[derive(Debug, Clone, Copy)]
pub enum MoveType {
    Rename,
    Copy,
}

#[test]
fn test_is_subpath() {
    assert!(is_subpath(RelativePath::new("")));
    assert!(is_subpath(RelativePath::new(".")));
    assert!(is_subpath(RelativePath::new("foo")));
    assert!(is_subpath(RelativePath::new("./foo/bar/baz.txt")));
    assert!(is_subpath(RelativePath::new("./foo/..")));
    assert!(is_subpath(RelativePath::new("foo/../baz.txt")));

    assert!(!is_subpath(RelativePath::new("..")));
    assert!(!is_subpath(RelativePath::new("./..")));
    assert!(!is_subpath(RelativePath::new("../foo")));
    assert!(!is_subpath(RelativePath::new("../foo/bar")));
    assert!(!is_subpath(RelativePath::new("../foo/bar/baz")));
    assert!(!is_subpath(RelativePath::new("foo/../..")));
    assert!(!is_subpath(RelativePath::new("foo/../../bar")));
}
