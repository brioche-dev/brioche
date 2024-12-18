use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet, VecDeque},
};

use anyhow::Context as _;
use joinery::JoinableIterator as _;

use crate::{
    recipe::{Artifact, Directory, WithMeta},
    Brioche,
};

/// Recursively walk a directory, attaching resources to directory entries
/// from discovered `brioche-resources.d` directories. Returns `true` if
/// the directory was changed.
pub async fn attach_resources(
    brioche: &Brioche,
    directory: &mut Directory,
    subpath: &[&bstr::BStr],
    mut resource_dirs: Cow<'_, [Directory]>,
) -> anyhow::Result<bool> {
    let mut changed = false;

    let mut entries = directory.entries(brioche).await?;

    // If there's a `brioche-resources.d` directory, remove it from the list
    // of entries to walk, and add it to the list of resource directories
    // to search.
    match entries.entry("brioche-resources.d".into()) {
        std::collections::btree_map::Entry::Occupied(resource_dir_entry) => {
            if matches!(resource_dir_entry.get(), Artifact::Directory(_)) {
                let Artifact::Directory(resource_dir) = resource_dir_entry.remove() else {
                    unreachable!();
                };

                resource_dirs.to_mut().push(resource_dir);
            }
        }
        std::collections::btree_map::Entry::Vacant(_) => {}
    }

    for (name, entry) in entries {
        let mut entry_subpath = subpath.to_vec();
        entry_subpath.push(bstr::BStr::new(&name));

        match entry {
            Artifact::File(mut file) => {
                // Try to find resources referenced by the file
                let file_resources =
                    file_resources_to_attach(brioche, &file, &entry_subpath, &resource_dirs)
                        .await?;

                // If the resources have changed, update the file's resources
                // then replace the old directory entry
                if file_resources != file.resources {
                    file.resources = file_resources;
                    directory
                        .insert(brioche, &name, Some(Artifact::File(file)))
                        .await?;
                    changed = true;
                }
            }
            Artifact::Symlink { .. } => {
                // Nothing to do for symlinks
            }
            Artifact::Directory(mut subdir) => {
                // Recursively attach resources within the subdirectory
                let subdir_changed = attach_resources(
                    brioche,
                    &mut subdir,
                    &entry_subpath,
                    Cow::Borrowed(&resource_dirs),
                );
                let subdir_changed = Box::pin(subdir_changed).await?;

                // If the subdirectory was updated, replace it in the directory
                if subdir_changed {
                    directory
                        .insert(brioche, &name, Some(Artifact::Directory(subdir)))
                        .await?;
                    changed = true;
                }
            }
        }
    }

    Ok(changed)
}

async fn file_resources_to_attach(
    brioche: &Brioche,
    file: &crate::recipe::File,
    entry_subpath: &[&bstr::BStr],
    resource_dirs: &[Directory],
) -> anyhow::Result<Directory> {
    let entry_subpath = entry_subpath.iter().join_with("/");
    let blob_path = crate::blob::blob_path(
        brioche,
        &mut crate::blob::get_save_blob_permit().await?,
        file.content_blob,
    )
    .await?;

    // Get any referenced resource paths from the pack, if any
    let extracted = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(blob_path)?;
        let extracted = brioche_pack::extract_pack(file).ok();

        anyhow::Ok(extracted)
    })
    .await??;
    let pack = extracted.map(|extracted| extracted.pack);

    let mut visited_resource_paths = HashSet::new();
    let mut pending_resource_paths: VecDeque<_> = pack
        .into_iter()
        .flat_map(|pack| pack.paths())
        .map(|path| (path, false))
        .collect();

    // Collect all referenced resources
    let mut file_resources = BTreeMap::new();
    while let Some((path, is_symlink_target)) = pending_resource_paths.pop_front() {
        // Skip resource paths we've already visited
        if !visited_resource_paths.insert(path.clone()) {
            continue;
        }

        // Get the file's existing resource if it has one
        let existing_resource = file.resources.get(brioche, &path).await?;

        // Get the resource from the resource directories to search
        let mut new_resource = None;
        for resource_dir in resource_dirs {
            if new_resource.is_some() {
                break;
            }

            new_resource = resource_dir.get(brioche, &path).await?;
        }

        // Ensure we found the resource
        let resource = match (existing_resource, new_resource) {
            (Some(resource), None) | (None, Some(resource)) => {
                // If we only found the existing resource or the new resource,
                // then we have the resource to add
                resource
            }
            (Some(existing), Some(new)) => {
                // If we found both the existing resource and the new resource,
                // ensure they match. If they don't, this means that the
                // new resource path is trying to shadow an existing resource
                anyhow::ensure!(
                    existing == new,
                    "conflicting resource in `{entry_subpath}`: resource `{path}` differs"
                );
                existing
            }
            (None, None) => {
                if !is_symlink_target {
                    // We didn't find the resource anywhere, so return an error.
                    // NOTE: This is currently more strict than how resources
                    // are added from a process recipe, so it might make sense
                    // to loosen this.
                    anyhow::bail!("resource `{path}` required by `{entry_subpath}` not found");
                } else {
                    // Skip broken symlink targets
                    tracing::debug!(
                        %entry_subpath,
                        resource_path = %path,
                        "symlink points into resource directory but the target could not be found, symlink may be broken"
                    );
                    continue;
                }
            }
        };

        match resource {
            Artifact::File(file) => {
                // Add the file to the file's resources
                file_resources.insert(path, WithMeta::without_meta(Artifact::File(file)));
            }
            Artifact::Symlink { target } => {
                // Disallow nested symlinks for now so we match
                // how resources are handled in process recipes.
                // TODO: Handle nested symlinks
                if is_symlink_target {
                    anyhow::bail!("resource `{path}` in `{entry_subpath}` is another symlink, which is not supported");
                }

                // Resolve the symlink path relative to the resource dir
                let mut target_path = path.clone();
                target_path.extend_from_slice(b"/../");
                target_path.extend_from_slice(&target);

                let target_path = crate::fs_utils::logical_path_bytes(&target_path).with_context(|| format!("failed to normalize symlink target `{target}` for resource `{path}` in `{entry_subpath}`"))?;

                // Add the symlink itself as a resource
                file_resources.insert(
                    path,
                    WithMeta::without_meta(Artifact::Symlink {
                        target: target.clone(),
                    }),
                );

                // Add the symlink target as a resource path
                pending_resource_paths.push_back((bstr::BString::from(target_path), true));
            }
            Artifact::Directory(directory) => {
                let entries = directory.entry_hashes();

                // Add an empty directory if there are no sub-entries to add
                if entries.is_empty() {
                    file_resources.insert(
                        path.clone(),
                        WithMeta::without_meta(Artifact::Directory(Directory::default())),
                    );
                }

                // Add each directory entry as a resource
                for name in entries.keys() {
                    let mut entry_path = path.clone();
                    entry_path.extend_from_slice(b"/");
                    entry_path.extend_from_slice(name);

                    pending_resource_paths.push_back((entry_path, is_symlink_target));
                }
            }
        }
    }

    // Build a directory from all the resources we found
    let file_resources = Directory::create(brioche, &file_resources).await?;
    Ok(file_resources)
}
