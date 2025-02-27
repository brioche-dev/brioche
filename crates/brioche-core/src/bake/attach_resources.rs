use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Context as _;

use crate::{
    Brioche,
    recipe::{Artifact, ArtifactDiscriminants, Directory},
};

/// Recursively walk a directory, attaching resources to directory entries
/// from discovered `brioche-resources.d` directories.
pub async fn attach_resources(brioche: &Brioche, directory: &mut Directory) -> anyhow::Result<()> {
    // Build a graph to plan resources to attach
    let plan = build_plan(brioche, directory).await?;

    // Sort nodes from the graph topologically. This gives us an order
    // of paths to update, so that each path is processed after all of its
    // dependencies are processed.
    let planned_nodes = petgraph::algo::toposort(petgraph::visit::Reversed(&plan.graph), None)
        .map_err(|error| {
            let cycle_node = &plan.graph[error.node_id()];
            anyhow::anyhow!("resource cycle detected in path: {}", cycle_node.path)
        })?;

    for node_index in planned_nodes {
        let node = &plan.graph[node_index];

        // Get the resources to attach based on graph edges. This only
        // applies to file nodes.
        let mut resources_to_attach = vec![];
        if node.kind == ArtifactDiscriminants::File {
            let edges_out = plan
                .graph
                .edges_directed(node_index, petgraph::Direction::Outgoing);

            for edge in edges_out {
                let AttachResourcesPlanEdge::InternalResource(resource) = &edge.weight();
                resources_to_attach.push(resource);
            }
        }

        // If there are no resources to attach, no need to update this node
        if resources_to_attach.is_empty() {
            continue;
        };

        // Get the artifact for this node. By this point, we know it should
        // be a file artifact.
        let artifact = directory
            .get(brioche, &node.path)
            .await?
            .with_context(|| format!("failed to get artifact `{}`", node.path))?;
        let Artifact::File(mut file) = artifact else {
            anyhow::bail!("expected `{}` to be a file", node.path);
        };

        let mut artifact_changed = false;

        for resource in resources_to_attach {
            // Get the resource artifact from the directory
            let resource_resolved_path = resource.resolved_path();
            let resource_artifact = directory
                .get(brioche, &resource_resolved_path)
                .await?
                .with_context(|| {
                    format!("failed to get resource `{}` for `{}` from resolved path `{resource_resolved_path}", resource.resource_path, node.path)
                })?;

            // Insert the new resource in the file's resources
            let replaced_resource = file
                .resources
                .insert(
                    brioche,
                    &resource.resource_path,
                    Some(resource_artifact.clone()),
                )
                .await?;

            match replaced_resource {
                None => {
                    // Added a new resource
                    artifact_changed = true;
                }
                Some(replaced_resource) => {
                    // Ensure that, if the resource already exists, it
                    // matches the newly-inserted resources.
                    // NOTE: This is currently more restrictive than
                    // how resources are handled by inputs, we may want
                    // to make this less strict.
                    anyhow::ensure!(
                        replaced_resource == resource_artifact,
                        "resource `{}` for `{}` did not match existing resource",
                        resource.resource_path,
                        node.path
                    );
                }
            }
        }

        // Insert the updated artifact back into the directory if it changed
        if artifact_changed {
            directory
                .insert(brioche, &node.path, Some(Artifact::File(file)))
                .await?;
        }
    }

    Ok(())
}

#[derive(Default)]
struct AttachResourcesPlan {
    graph: petgraph::graph::DiGraph<AttachResourcesPlanNode, AttachResourcesPlanEdge>,
    paths_to_nodes: HashMap<bstr::BString, petgraph::prelude::NodeIndex>,
}

#[derive(Debug, Clone)]
struct AttachResourcesPlanNode {
    path: bstr::BString,
    kind: ArtifactDiscriminants,
}

#[derive(Debug, Clone)]
enum AttachResourcesPlanEdge {
    InternalResource(ResolvedResourcePath),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResolvedResourcePath {
    resource_dir_path: bstr::BString,
    resource_path: bstr::BString,
    resource_kind: ArtifactDiscriminants,
}

impl ResolvedResourcePath {
    fn resolved_path(&self) -> bstr::BString {
        let mut resolved_path = self.resource_dir_path.clone();
        resolved_path.extend_from_slice(b"/");
        resolved_path.extend_from_slice(&self.resource_path);
        resolved_path
    }
}

/// Plan resources to attach to artifacts within a directory by building
/// a graph.
async fn build_plan(
    brioche: &Brioche,
    directory: &Directory,
) -> anyhow::Result<AttachResourcesPlan> {
    let mut plan = AttachResourcesPlan::default();
    let mut visited: HashSet<bstr::BString> = HashSet::new();

    // Start with each entry of the directory
    let entries = directory.entries(brioche).await?;
    let mut queue: VecDeque<_> = entries.into_iter().collect();

    while let Some((subpath, artifact)) = queue.pop_front() {
        // Skip nodes we've already processed
        if !visited.insert(subpath.clone()) {
            continue;
        }

        // Add a new node for this entry if one doesn't exist
        let node = *plan
            .paths_to_nodes
            .entry(subpath.clone())
            .or_insert_with(|| {
                plan.graph.add_node(AttachResourcesPlanNode {
                    path: subpath.clone(),
                    kind: ArtifactDiscriminants::from(&artifact),
                })
            });

        match artifact {
            Artifact::File(file) => {
                // Get the resources directly referenced by this file, which
                // we resolved within the directory
                let direct_resources =
                    resolve_internal_file_resources(brioche, directory, &subpath, &file).await?;

                // Get any indirect resources by these resources (e.g. symlinks)
                let indirect_resources =
                    resolve_indirect_resources(brioche, directory, &subpath, &direct_resources)
                        .await?;

                let resources = direct_resources.into_iter().chain(indirect_resources);

                // Add an edge for each direct and indirect resource
                for resource in resources {
                    let resolved_path = resource.resolved_path();
                    let resource_node = *plan
                        .paths_to_nodes
                        .entry(resolved_path.clone())
                        .or_insert_with(|| {
                            plan.graph.add_node(AttachResourcesPlanNode {
                                path: resolved_path,
                                kind: resource.resource_kind,
                            })
                        });
                    plan.graph.update_edge(
                        node,
                        resource_node,
                        AttachResourcesPlanEdge::InternalResource(resource),
                    );
                }
            }
            Artifact::Symlink { .. } => {}
            Artifact::Directory(subdirectory) => {
                let entries = subdirectory.entries(brioche).await?;

                // Enqueue each directory entry so we include them in the plan
                for (name, entry) in entries {
                    let mut entry_subpath = subpath.clone();
                    entry_subpath.extend_from_slice(b"/");
                    entry_subpath.extend_from_slice(&name);

                    queue.push_back((entry_subpath, entry));
                }
            }
        }
    }

    Ok(plan)
}

/// Resolve all the resources needed by a file within the given directory.
/// Each resource will be resolved by traversing up the directory, or
/// must already be present within the file's resources.
async fn resolve_internal_file_resources(
    brioche: &Brioche,
    directory: &Directory,
    subpath: &[u8],
    file: &crate::recipe::File,
) -> anyhow::Result<Vec<ResolvedResourcePath>> {
    let subpath = bstr::BStr::new(subpath);

    // Get the file's blob
    let blob_path = crate::blob::blob_path(
        brioche,
        &mut crate::blob::get_save_blob_permit().await?,
        file.content_blob,
    )
    .await?;

    // Try to extract a pack from the file's blob, and get its resource paths
    // if it has any
    let extracted = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(blob_path)?;
        let extracted = brioche_pack::extract_pack(file).ok();

        anyhow::Ok(extracted)
    })
    .await??;
    let pack = extracted.map(|extracted| extracted.pack);
    let resource_paths = pack.into_iter().flat_map(|pack| pack.paths());

    let mut resolved_resource_paths = vec![];

    for resource_path in resource_paths {
        // Find the resource internally by traversing up the directory
        let resolved_resource =
            resolve_internal_resource(brioche, directory, subpath, &resource_path).await?;

        // Get the existing resource attached to the file
        let existing_resource = file.resources.get(brioche, &resource_path).await?;

        match (resolved_resource, existing_resource) {
            (Some(resolved), _) => {
                // We resolved the resource internally, so add it
                // to the list
                resolved_resource_paths.push(ResolvedResourcePath {
                    resource_dir_path: resolved.resource_dir_path.clone(),
                    resource_path,
                    resource_kind: ArtifactDiscriminants::from(&resolved.artifact),
                });
            }
            (None, Some(_)) => {
                // Resource not found internally, but the resource already
                // exists
            }
            (None, None) => {
                // Resource not found internally and it wasn't already
                // present on the file!
                anyhow::bail!("resource `{resource_path}` required by `{subpath}` not found");
            }
        }
    }

    Ok(resolved_resource_paths)
}

/// From a list of resources, get any additional resources that are
/// used indirectly (namely, as symlink targets from other resources).
async fn resolve_indirect_resources(
    brioche: &Brioche,
    directory: &Directory,
    referrer_subpath: &[u8],
    resources: &[ResolvedResourcePath],
) -> anyhow::Result<Vec<ResolvedResourcePath>> {
    let referrer_subpath = bstr::BStr::new(referrer_subpath);

    // Start by visiting each of the provided resources
    let mut resource_queue: VecDeque<_> = resources.iter().cloned().collect();
    let mut visited_resources = HashSet::<ResolvedResourcePath>::new();
    let mut indirect_resources = vec![];

    while let Some(resource) = resource_queue.pop_front() {
        if !visited_resources.insert(resource.clone()) {
            continue;
        }

        // Get the resource artifact
        let resource_resolved_path = resource.resolved_path();
        let artifact = directory.get(brioche, &resource_resolved_path).await?;
        let artifact = artifact.with_context(|| {
            format!(
                "failed to get resource `{}` from `{referrer_subpath}`",
                resource.resource_path
            )
        })?;

        match artifact {
            Artifact::File(_) => {
                // No indirect resources to get
            }
            Artifact::Symlink { target } => {
                // Get the resource directory containing the current resource
                let resource_dir = directory.get(brioche, &resource.resource_dir_path).await?;
                let Some(Artifact::Directory(resource_dir)) = resource_dir else {
                    anyhow::bail!(
                        "failed to get resource directory for resource `{}` from `{referrer_subpath}`",
                        resource.resource_path,
                    );
                };

                // Get the target path relative to the resource directory
                let mut target_path = resource.resource_path.clone();
                target_path.extend_from_slice(b"/../");
                target_path.extend_from_slice(&target);
                let target_path = crate::fs_utils::logical_path_bytes(&target_path).ok();

                if let Some(target_path) = target_path {
                    // Try to resolve the target path from the resource
                    // directory. We check against the resource directory
                    // directly so we don't traverse outside
                    let target_path = bstr::BString::new(target_path);
                    let target_artifact = resource_dir.get(brioche, &target_path).await?;

                    match target_artifact {
                        Some(Artifact::Symlink { .. }) => {
                            // TODO: Handle nested symlinks
                            anyhow::bail!(
                                "target of symlink {} is another symlink, which is not supported",
                                resource.resource_path,
                            );
                        }
                        Some(target_artifact) => {
                            // Found a valid symlink target! This is an
                            // indirect resource
                            let indirect_resource = ResolvedResourcePath {
                                resource_dir_path: resource.resource_dir_path.clone(),
                                resource_path: target_path.clone(),
                                resource_kind: ArtifactDiscriminants::from(&target_artifact),
                            };

                            // Add it to the list of indirect resources,
                            // and queue it so we find more indirect resources
                            indirect_resources.push(indirect_resource.clone());
                            resource_queue.push_back(indirect_resource);
                        }
                        None => {
                            // Broken symlink, ignore it
                        }
                    }
                }
            }
            Artifact::Directory(directory) => {
                // Queue each directory entry to look for more
                // indirect resources
                let entries = directory.entries(brioche).await?;
                for (name, entry) in entries {
                    let mut entry_path = resource.resource_path.clone();
                    entry_path.extend_from_slice(b"/");
                    entry_path.extend_from_slice(&name);

                    resource_queue.push_back(ResolvedResourcePath {
                        resource_dir_path: resource.resource_dir_path.clone(),
                        resource_path: entry_path,
                        resource_kind: ArtifactDiscriminants::from(&entry),
                    });
                }
            }
        }
    }

    Ok(indirect_resources)
}

struct ResolvedResource {
    resource_dir_path: bstr::BString,
    artifact: Artifact,
}

/// Find the resource `resource_path` by traversing starting from `subpath`.
async fn resolve_internal_resource(
    brioche: &Brioche,
    directory: &Directory,
    subpath: &[u8],
    resource_path: &[u8],
) -> anyhow::Result<Option<ResolvedResource>> {
    // Normalize the provided path, and start the search from there
    let current_path = crate::fs_utils::logical_path_bytes(subpath)?;
    let mut current_path = bstr::BString::new(current_path);

    loop {
        // Get the parent directory path
        let mut parent_path = current_path;
        parent_path.extend_from_slice(b"/..");
        let parent_path = crate::fs_utils::logical_path_bytes(&parent_path);
        let Ok(parent_path) = parent_path else {
            return Ok(None);
        };
        let parent_path = bstr::BString::new(parent_path);
        current_path = parent_path;

        // Determine the resource directory path to search
        let mut resource_dir_path = current_path.clone();
        resource_dir_path.extend_from_slice(b"/brioche-resources.d");
        let resource_dir_path = crate::fs_utils::logical_path_bytes(&resource_dir_path).ok();
        let Some(resource_dir_path) = resource_dir_path else {
            continue;
        };

        // Try to get the resource directory path if it exists
        let resource_dir = directory.get(brioche, &resource_dir_path).await?;
        let Some(Artifact::Directory(resource_dir)) = resource_dir else {
            continue;
        };

        let artifact = resource_dir.get(brioche, resource_path).await?;
        if let Some(artifact) = artifact {
            // This resource directory contains the resource, so we're done
            return Ok(Some(ResolvedResource {
                resource_dir_path: bstr::BString::from(resource_dir_path),
                artifact,
            }));
        }
    }
}
