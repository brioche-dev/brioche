use std::{
    os::unix::prelude::OpenOptionsExt as _,
    path::{Path, PathBuf},
};

pub fn add_named_blob(
    resource_dir: &Path,
    mut contents: impl std::io::Seek + std::io::Read,
    executable: bool,
    name: impl AsRef<Path>,
) -> Result<PathBuf, AddBlobError> {
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut contents, &mut hasher)?;
    let hash = hasher.finalize();

    let blob_suffix = if executable { ".x" } else { "" };
    let blob_name = format!("{hash}{blob_suffix}");

    contents.seek(std::io::SeekFrom::Start(0))?;

    let blob_dir = resource_dir.join("blobs");
    let blob_path = blob_dir.join(&blob_name);
    let blob_temp_id = ulid::Ulid::new();
    let blob_temp_path = blob_dir.join(format!("{blob_name}-{blob_temp_id}"));
    std::fs::create_dir_all(&blob_dir)?;

    let mut blob_file_options = std::fs::OpenOptions::new();
    blob_file_options.create_new(true).write(true);
    if executable {
        blob_file_options.mode(0o777);
    }
    let mut blob_file = blob_file_options.open(&blob_temp_path)?;
    std::io::copy(&mut contents, &mut blob_file)?;
    drop(blob_file);
    std::fs::rename(&blob_temp_path, &blob_path)?;

    let alias_dir = resource_dir.join("aliases").join(&blob_name);
    std::fs::create_dir_all(&alias_dir)?;

    let alias_path = alias_dir.join(name);
    let _ = std::fs::remove_file(&alias_path);
    let blob_pack_relative_path = pathdiff::diff_paths(&blob_path, &alias_dir)
        .expect("blob path is not a prefix of alias path");
    std::os::unix::fs::symlink(blob_pack_relative_path, &alias_path)?;

    let alias_path = alias_path
        .strip_prefix(resource_dir)
        .expect("alias path is not in resources dir");
    Ok(alias_path.to_owned())
}

#[derive(Debug, thiserror::Error)]
pub enum AddBlobError {
    #[error("{0}")]
    IoError(#[from] std::io::Error),
}
