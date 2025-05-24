use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use bstr::{ByteSlice as _, ByteVec as _};
use petgraph::visit::EdgeRef as _;
use tracing::Instrument as _;

use crate::fs_utils::{is_executable, logical_path_bytes, set_directory_rwx_recursive};

use super::{
    Brioche,
    recipe::{Artifact, Directory, File, Meta, WithMeta},
};

pub struct InputOptions<'a> {
    pub input_path: &'a Path,
    pub remove_input: bool,
    pub resource_dir: Option<&'a Path>,
    pub input_resource_dirs: &'a [PathBuf],
    pub saved_paths: &'a mut HashMap<PathBuf, Artifact>,
    pub meta: &'a Arc<Meta>,
}

#[tracing::instrument(skip_all, err, fields(input_path = %options.input_path.display()))]
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

    // Create a plan for the input but building a graph. This ensures that we
    // can avoid reading the same files and resources repeatedly. This is done
    // in a blocking task since it relies on lots of file I/O.
    tracing::debug!(input_path = %options.input_path.display(), "creating plan for input");
    let (plan, root_node) = tokio::task::spawn_blocking({
        let input_path = options.input_path.to_owned();
        let remove_input = options.remove_input;
        let resource_dir = options.resource_dir.map(|path| path.to_owned());
        let input_resource_dirs = options.input_resource_dirs.to_owned();
        let meta = options.meta.clone();
        move || {
            let mut plan = CreateInputPlan::default();

            // Add nodes and edges for all files/directories/symlinks
            let root_node = add_input_plan_nodes(
                &mut InputOptions {
                    input_path: &input_path,
                    remove_input,
                    resource_dir: resource_dir.as_deref(),
                    input_resource_dirs: &input_resource_dirs,
                    saved_paths: &mut Default::default(),
                    meta: &meta,
                },
                &mut plan,
            )?;

            // Add edges for indirect resources
            add_input_plan_indirect_resources(&mut plan)?;

            anyhow::Ok((plan, root_node))
        }
    })
    .await??;

    tracing::debug!(input_path = %options.input_path.display(), "loading blobs");

    // Find all file nodes, each of which should be saved as a blob
    let file_nodes = plan
        .graph
        .node_indices()
        .filter(|node| matches!(plan.graph[*node], CreateInputPlanNode::File { .. }));

    // Save all file nodes as blobs, each in a separate task. The permit will
    // limit the maximum concurrency.
    let mut nodes_to_blobs_tasks = vec![];
    for node in file_nodes {
        let path = plan.nodes_to_paths[&node].clone();
        let brioche = brioche.clone();
        let remove_blob = plan.paths_to_remove.contains(&path);

        let task = async move {
            let result = tokio::spawn(
                async move {
                    let mut blob_permit = super::blob::get_save_blob_permit().await?;
                    let blob = super::blob::save_blob_from_file(
                        &brioche,
                        &mut blob_permit,
                        &path,
                        super::blob::SaveBlobOptions::default().remove_input(remove_blob),
                        &mut Default::default(),
                    )
                    .await?;

                    anyhow::Ok((node, blob))
                }
                .instrument(tracing::Span::current()),
            )
            .await??;

            anyhow::Ok(result)
        }
        .instrument(tracing::Span::current());

        nodes_to_blobs_tasks.push(task);
    }

    let nodes_to_blobs: HashMap<_, _> = futures::future::try_join_all(nodes_to_blobs_tasks)
        .await?
        .into_iter()
        .collect();

    let mut nodes_to_artifacts = HashMap::<petgraph::graph::NodeIndex, WithMeta<Artifact>>::new();

    // Order the nodes in topological order, but with the edges reversed. This
    // ensures that we process each node after all of its dependencies have
    // been processed.
    let planned_nodes = petgraph::algo::toposort(petgraph::visit::Reversed(&plan.graph), None)
        .map_err(|_| anyhow::anyhow!("cycle detected in input {:?}", options.input_path))?;

    tracing::debug!(input_path = %options.input_path.display(), "creating nodes");

    // Create an artifact for each node
    for node_index in planned_nodes {
        let node = &plan.graph[node_index];
        let path = plan.nodes_to_paths[&node_index].clone();

        let artifact = match node {
            CreateInputPlanNode::File { is_executable } => {
                let content_blob = nodes_to_blobs
                    .get(&node_index)
                    .ok_or_else(|| anyhow::anyhow!("blob not found for file node: {:?}", path))?;

                let mut resources = Directory::default();

                // The file should have resource (or indirect resource) edges;
                // use each one to build the resources directory for the
                // file artifact
                let edges_out = plan
                    .graph
                    .edges_directed(node_index, petgraph::Direction::Outgoing);
                for edge in edges_out {
                    let resource_path = match &edge.weight() {
                        CreateInputPlanEdge::Resource { path } => path,
                        CreateInputPlanEdge::IndirectResource { path } => path,
                        _ => anyhow::bail!("unexpected edge for file node: {:?}", edge.weight()),
                    };

                    let resource_node_index = edge.target();
                    let resource_artifact = nodes_to_artifacts
                        .get(&resource_node_index)
                        .map(|artifact| &artifact.value)
                        .ok_or_else(|| anyhow::anyhow!("resource node not found"))?;

                    resources
                        .insert(brioche, resource_path, Some(resource_artifact.clone()))
                        .await?;
                }

                Artifact::File(File {
                    content_blob: *content_blob,
                    executable: *is_executable,
                    resources,
                })
            }
            CreateInputPlanNode::Symlink { target } => {
                // Artifacts don't need any special handling. Symlink targets
                // are only used for adding indirect resource edges to files
                Artifact::Symlink {
                    target: target.clone(),
                }
            }
            CreateInputPlanNode::Directory => {
                let mut directory = Directory::default();

                // The directory should have directory entry edges, so use each
                // one to build the directory from its entries
                let edges_out = plan
                    .graph
                    .edges_directed(node_index, petgraph::Direction::Outgoing);
                for edge in edges_out {
                    let CreateInputPlanEdge::DirectoryEntry {
                        file_name: entry_name,
                    } = &edge.weight()
                    else {
                        anyhow::bail!("unexpected edge for directory node: {:?}", edge.weight())
                    };

                    let entry_node_index = edge.target();
                    let entry_artifact = nodes_to_artifacts
                        .get(&entry_node_index)
                        .map(|artifact| &artifact.value)
                        .ok_or_else(|| anyhow::anyhow!("directory entry node not found"))?;

                    directory
                        .insert(brioche, entry_name, Some(entry_artifact.clone()))
                        .await?;
                }

                Artifact::Directory(directory)
            }
        };

        nodes_to_artifacts.insert(node_index, WithMeta::new(artifact, options.meta.clone()));
    }

    tracing::debug!(input_path = %options.input_path.display(), "finished creating input");

    let artifact = nodes_to_artifacts
        .remove(&root_node)
        .ok_or_else(|| anyhow::anyhow!("root node not found"))?;

    // Remove the input if the `remove_input` option is set. We've already
    // removed some parts of the input (such as individual files while
    // saving blobs), but this ensures nothing is left over.
    if options.remove_input {
        tracing::debug!(input_path = %options.input_path.display(), "removing input");
        crate::fs_utils::try_remove(options.input_path).await?;
    }

    Ok(artifact)
}

pub struct FoundResource<'a> {
    pub path: PathBuf,
    pub metadata: std::fs::Metadata,
    pub resource_dir: &'a Path,
}

fn find_resource_sync<'a>(
    resource_dir: Option<&'a Path>,
    input_resource_dirs: &'a [PathBuf],
    subpath: &Path,
) -> anyhow::Result<Option<FoundResource<'a>>> {
    let resource_dirs = resource_dir
        .into_iter()
        .chain(input_resource_dirs.iter().map(|dir| &**dir));

    for resource_dir in resource_dirs {
        let resource_path = resource_dir.join(subpath);
        let resource_metadata = std::fs::symlink_metadata(&resource_path);
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

#[derive(Debug, Clone)]
enum CreateInputPlanNode {
    File { is_executable: bool },
    Directory,
    Symlink { target: bstr::BString },
}

#[derive(Debug, Clone)]
enum CreateInputPlanEdge {
    DirectoryEntry { file_name: bstr::BString },
    Resource { path: bstr::BString },
    IndirectResource { path: bstr::BString },
    SymlinkTarget,
}

#[derive(Default)]
struct CreateInputPlan {
    graph: petgraph::graph::DiGraph<CreateInputPlanNode, CreateInputPlanEdge>,
    paths_to_nodes: HashMap<PathBuf, petgraph::graph::NodeIndex>,
    nodes_to_paths: HashMap<petgraph::graph::NodeIndex, PathBuf>,
    paths_to_remove: HashSet<PathBuf>,
}

fn add_input_plan_nodes(
    options: &mut InputOptions,
    plan: &mut CreateInputPlan,
) -> anyhow::Result<petgraph::graph::NodeIndex> {
    if let Some(node) = plan.paths_to_nodes.get(options.input_path) {
        return Ok(*node);
    }

    let metadata =
        std::fs::symlink_metadata(options.input_path).context("failed to get metadata")?;

    let root_node = if metadata.is_file() {
        let is_executable = is_executable(&metadata.permissions());
        let node_index = plan
            .graph
            .add_node(CreateInputPlanNode::File { is_executable });

        plan.paths_to_nodes
            .insert(options.input_path.to_owned(), node_index);
        plan.nodes_to_paths
            .insert(node_index, options.input_path.to_owned());

        if options.remove_input {
            plan.paths_to_remove.insert(options.input_path.to_owned());
        }

        let mut file = std::fs::File::open(options.input_path)
            .with_context(|| format!("failed to open file {:?}", options.input_path))?;
        let extracted = brioche_pack::extract_pack(&mut file).ok();
        let pack = extracted.map(|extracted| extracted.pack);
        let resource_paths = pack.into_iter().flat_map(|pack| pack.paths());
        for resource_path in resource_paths {
            let resource_subpath = resource_path.to_path().context("invalid resource path")?;
            let resource = find_resource_sync(
                options.resource_dir,
                options.input_resource_dirs,
                resource_subpath,
            )?;

            if let Some(resource) = resource {
                let resource_node = add_input_plan_nodes(
                    &mut InputOptions {
                        input_path: &resource.path,
                        remove_input: false,
                        resource_dir: options.resource_dir,
                        input_resource_dirs: options.input_resource_dirs,
                        saved_paths: options.saved_paths,
                        meta: options.meta,
                    },
                    plan,
                )?;

                plan.graph.update_edge(
                    node_index,
                    resource_node,
                    CreateInputPlanEdge::Resource {
                        path: resource_path,
                    },
                );
            } else {
                tracing::debug!(input_path = %options.input_path.display(), %resource_path, "could not find resource for file, skipping");
            }
        }

        node_index
    } else if metadata.is_symlink() {
        let input_parent_path = options
            .input_path
            .parent()
            .context("failed to get symlink parent directory")?;
        let target_path =
            std::fs::read_link(options.input_path).context("failed to read symlink target")?;
        let target = <Vec<u8>>::from_path_buf(target_path.clone()).map_err(|_| {
            anyhow::anyhow!("invalid symlink target at {}", options.input_path.display())
        })?;
        let target = bstr::BString::from(target);

        let node_index = plan.graph.add_node(CreateInputPlanNode::Symlink { target });

        plan.paths_to_nodes
            .insert(options.input_path.to_owned(), node_index);
        plan.nodes_to_paths
            .insert(node_index, options.input_path.to_owned());

        let resource_dirs = options
            .resource_dir
            .iter()
            .copied()
            .chain(options.input_resource_dirs.iter().map(|dir| &**dir));
        let canonical_resource_dirs = resource_dirs
            .map(std::fs::canonicalize)
            .collect::<Result<Vec<_>, _>>()
            .context("failed to canonicalize resource dirs")?;

        // Logically resolve the symlink target path
        let resolved_path = input_parent_path.join(&target_path);
        let resolved_path = crate::fs_utils::logical_path(&resolved_path);

        // If the resolved path is within a resource directory, get the
        // resource dir and the subpath
        let resource_dir_with_subpath = resolved_path.ancestors().find_map(|ancestor| {
            let subpath = resolved_path.strip_prefix(ancestor).ok()?;
            let canonical_ancestor = std::fs::canonicalize(ancestor).ok()?;

            let ancestor_is_resource_dir = canonical_resource_dirs.contains(&canonical_ancestor);

            if ancestor_is_resource_dir {
                Some((canonical_ancestor, subpath))
            } else {
                None
            }
        });

        // If the resolved path is in a resource directory, get the
        // metadata of the target
        let target_info = resource_dir_with_subpath.and_then(|(resource_dir, subpath)| {
            let target_metadata = std::fs::symlink_metadata(&resolved_path)
                .inspect_err(|error| {
                    tracing::debug!(
                        input_path = %options.input_path.display(),
                        resolved_path = %resolved_path.display(),
                        %error,
                        "symlink points into resource directory but the target could not be read, symlink may be broken"
                    );
                })
                .ok()?;
            Some((resource_dir, subpath, target_metadata))
        });

        // If the symlink's target could be resolved within a resource
        // directory and if we could read the target's metadata, then we
        // add a node for the target and add an edge to it. This should
        // apply to most resource symlinks, but other symlinks may be broken
        // or e.g. could intentionally point to an absolute path
        if let Some((resource_dir, subpath, target_metadata)) = target_info {
            // TODO: Handle nested symlinks
            anyhow::ensure!(
                !target_metadata.is_symlink(),
                "target of symlink {} is another symlink, which is not supported",
                options.input_path.display(),
            );

            for ancestor in subpath.ancestors().skip(1) {
                let ancestor_path = resource_dir.join(ancestor);
                let ancestor_metadata = std::fs::symlink_metadata(&ancestor_path)
                    .context("failed to get ancestor metadata")?;

                // TODO: Handle traversing through directory symlinks
                anyhow::ensure!(
                    !ancestor_metadata.is_symlink(),
                    "symlink {} traverses through a symlink path, which is not supported",
                    options.input_path.display(),
                );
            }

            let target_node = add_input_plan_nodes(
                &mut InputOptions {
                    input_path: &resolved_path,
                    remove_input: false,
                    resource_dir: options.resource_dir,
                    input_resource_dirs: options.input_resource_dirs,
                    saved_paths: options.saved_paths,
                    meta: options.meta,
                },
                plan,
            )?;

            plan.graph
                .update_edge(node_index, target_node, CreateInputPlanEdge::SymlinkTarget);
        }

        node_index
    } else if metadata.is_dir() {
        let node_index = plan.graph.add_node(CreateInputPlanNode::Directory);

        plan.paths_to_nodes
            .insert(options.input_path.to_owned(), node_index);
        plan.nodes_to_paths
            .insert(node_index, options.input_path.to_owned());

        let dir = std::fs::read_dir(options.input_path)?;
        for dir_entry in dir {
            let dir_entry = dir_entry?;
            let file_name = dir_entry.file_name();
            let file_name = <Vec<u8>>::from_os_string(file_name).map_err(|_| {
                anyhow::anyhow!(
                    "invalid file name {:?} in directory {}",
                    dir_entry.file_name(),
                    options.input_path.display()
                )
            })?;
            let file_name = bstr::BString::from(file_name);

            let dir_entry_node = add_input_plan_nodes(
                &mut InputOptions {
                    input_path: &dir_entry.path(),
                    remove_input: false,
                    resource_dir: options.resource_dir,
                    input_resource_dirs: options.input_resource_dirs,
                    saved_paths: options.saved_paths,
                    meta: options.meta,
                },
                plan,
            )?;

            plan.graph.update_edge(
                node_index,
                dir_entry_node,
                CreateInputPlanEdge::DirectoryEntry { file_name },
            );
        }

        node_index
    } else {
        anyhow::bail!("unsupported file type at {:?}", options.input_path);
    };

    Ok(root_node)
}

fn add_input_plan_indirect_resources(plan: &mut CreateInputPlan) -> anyhow::Result<()> {
    let mut resource_paths: Vec<_> = plan
        .graph
        .edge_references()
        .filter_map(|edge| {
            let CreateInputPlanEdge::Resource {
                path: resource_path,
            } = edge.weight()
            else {
                return None;
            };

            let node = edge.source();
            let resource_node = edge.target();

            Some((node, resource_path.clone(), resource_node))
        })
        .collect();
    let mut visited_resource_paths = HashSet::new();

    let mut indirect_resources = vec![];

    while let Some(visit) = resource_paths.pop() {
        if !visited_resource_paths.insert(visit.clone()) {
            continue;
        }

        let (node, resource_path, resource_node) = visit;

        for subresource_edge in plan.graph.edges(resource_node) {
            match subresource_edge.weight() {
                CreateInputPlanEdge::DirectoryEntry { file_name } => {
                    let mut subresource_path = resource_path.clone();
                    subresource_path.extend_from_slice(b"/");
                    subresource_path.extend_from_slice(file_name);

                    let subresource_node = subresource_edge.target();

                    resource_paths.push((node, subresource_path, subresource_node));
                }
                CreateInputPlanEdge::Resource { path }
                | CreateInputPlanEdge::IndirectResource { path } => {
                    let subresource_node = subresource_edge.target();

                    resource_paths.push((node, path.clone(), subresource_node));
                }
                CreateInputPlanEdge::SymlinkTarget => {
                    let CreateInputPlanNode::Symlink { target } = &plan.graph[resource_node] else {
                        anyhow::bail!("non-symlink node has symlink target edge");
                    };

                    let mut target_path = resource_path.clone();
                    target_path.extend_from_slice(b"/../");
                    target_path.extend_from_slice(target);

                    let target_path =
                        logical_path_bytes(&target_path).context("invalid symlink target")?;
                    let target_path = bstr::BString::from(target_path);

                    let target_node = subresource_edge.target();

                    indirect_resources.push((
                        node,
                        target_node,
                        CreateInputPlanEdge::IndirectResource {
                            path: target_path.clone(),
                        },
                    ));

                    resource_paths.push((node, target_path, target_node));
                }
            }
        }
    }

    for (node, target_node, weight) in indirect_resources {
        plan.graph.update_edge(node, target_node, weight);
    }

    Ok(())
}
