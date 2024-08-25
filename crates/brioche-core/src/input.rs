use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use bstr::{ByteSlice as _, ByteVec as _};
use petgraph::visit::EdgeRef as _;

use crate::fs_utils::{is_executable, logical_path_bytes, set_directory_rwx_recursive};

use super::{
    recipe::{Artifact, Directory, File, Meta, WithMeta},
    Brioche,
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

    let result = create_input_inner(brioche, options, &mut Vec::new()).await?;
    Ok(result)
}

#[tracing::instrument(skip_all, err, fields(input_path = %options.input_path.display()))]
pub async fn create_input_inner(
    brioche: &Brioche,
    options: InputOptions<'_>,
    buffer: &mut Vec<u8>,
) -> anyhow::Result<WithMeta<Artifact>> {
    tracing::debug!(input_path = %options.input_path.display(), "creating plan for input");
    let (plan, root_node) = tokio::task::spawn_blocking({
        let input_path = options.input_path.to_owned();
        let remove_input = options.remove_input;
        let resource_dir = options.resource_dir.map(|path| path.to_owned());
        let input_resource_dirs = options.input_resource_dirs.to_owned();
        let meta = options.meta.clone();
        move || {
            let mut plan = CreateInputPlan::default();
            let root_node = add_input_plan_nodes(
                InputOptions {
                    input_path: &input_path,
                    remove_input,
                    resource_dir: resource_dir.as_deref(),
                    input_resource_dirs: &input_resource_dirs,
                    saved_paths: &mut Default::default(),
                    meta: &meta,
                },
                &mut plan,
            )?;

            add_input_plan_indirect_resources(&mut plan)?;

            anyhow::Ok((plan, root_node))
        }
    })
    .await??;

    tracing::debug!(input_path = %options.input_path.display(), "loading blobs");

    let file_nodes = plan
        .graph
        .node_indices()
        .filter(|node| matches!(plan.graph[*node], CreateInputPlanNode::File { .. }));

    let mut nodes_to_blobs_tasks = vec![];
    for node in file_nodes {
        let path = plan.nodes_to_paths[&node].to_owned();
        let brioche = brioche.clone();
        let remove_blob = plan.paths_to_remove.contains(&path);

        let task = async move {
            let result = tokio::spawn(async move {
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
            })
            .await??;

            anyhow::Ok(result)
        };

        nodes_to_blobs_tasks.push(task);
    }

    let nodes_to_blobs: HashMap<_, _> = futures::future::try_join_all(nodes_to_blobs_tasks)
        .await?
        .into_iter()
        .collect();

    let mut nodes_to_artifacts = HashMap::<petgraph::graph::NodeIndex, WithMeta<Artifact>>::new();
    let planned_nodes = petgraph::algo::toposort(petgraph::visit::Reversed(&plan.graph), None)
        .map_err(|_| anyhow::anyhow!("cycle detected in input {:?}", options.input_path))?;

    tracing::debug!(input_path = %options.input_path.display(), "creating nodes");

    for node_index in planned_nodes {
        let node = &plan.graph[node_index];
        let path = plan.nodes_to_paths[&node_index].to_owned();

        let artifact = match node {
            CreateInputPlanNode::File { is_executable } => {
                let content_blob = nodes_to_blobs
                    .get(&node_index)
                    .ok_or_else(|| anyhow::anyhow!("blob not found for file node: {:?}", path))?;

                let mut resources = Directory::default();

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
            CreateInputPlanNode::Symlink { target } => Artifact::Symlink {
                target: target.clone(),
            },
            CreateInputPlanNode::Directory => {
                let mut directory = Directory::default();

                let edges_out = plan
                    .graph
                    .edges_directed(node_index, petgraph::Direction::Outgoing);
                for edge in edges_out {
                    let entry_name = match &edge.weight() {
                        CreateInputPlanEdge::DirectoryEntry { file_name } => file_name,
                        _ => {
                            anyhow::bail!("unexpected edge for directory node: {:?}", edge.weight())
                        }
                    };

                    let entry_node_index = edge.target();
                    let entry_artifact = nodes_to_artifacts
                        .get(&entry_node_index)
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

    tracing::debug!(input_path = %options.input_path.display(), "finished");

    let artifact = nodes_to_artifacts
        .remove(&root_node)
        .ok_or_else(|| anyhow::anyhow!("root node not found"))?;
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
    options: InputOptions,
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
            )?
            .with_context(|| format!("resource not found: {resource_path:?}"))?;

            let resource_node = add_input_plan_nodes(
                InputOptions {
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
        }

        node_index
    } else if metadata.is_symlink() {
        // TODO: Handle broken links

        let target =
            std::fs::read_link(options.input_path).context("failed to read symlink target")?;
        let target = <Vec<u8>>::from_path_buf(target).map_err(|_| {
            anyhow::anyhow!("invalid symlink target at {}", options.input_path.display())
        })?;
        let target = bstr::BString::from(target);

        let node_index = plan.graph.add_node(CreateInputPlanNode::Symlink { target });

        plan.paths_to_nodes
            .insert(options.input_path.to_owned(), node_index);
        plan.nodes_to_paths
            .insert(node_index, options.input_path.to_owned());

        let canonical_path = std::fs::canonicalize(options.input_path)
            .context("failed to canonicalize symlink path")?;
        let resource_dirs = options
            .resource_dir
            .into_iter()
            .chain(options.input_resource_dirs.iter().map(|dir| &**dir));
        for resource_dir in resource_dirs {
            let canonical_resource_dir = std::fs::canonicalize(resource_dir)
                .context("failed to canonicalize resource dir")?;
            if canonical_path.starts_with(&canonical_resource_dir) {
                let target_node = add_input_plan_nodes(
                    InputOptions {
                        input_path: &canonical_path,
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
                InputOptions {
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
            let resource_path = match edge.weight() {
                CreateInputPlanEdge::Resource { path } => path,
                _ => return None,
            };

            let node = edge.source();
            let resource_node = edge.target();

            Some((node, resource_path.clone(), resource_node))
        })
        .collect();

    let mut visited_resources = HashSet::new();
    let mut indirect_resources = vec![];

    while let Some((node, resource_path, resource_node)) = resource_paths.pop() {
        if !visited_resources.insert(resource_path.clone()) {
            continue;
        }

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
                    let target = match &plan.graph[resource_node] {
                        CreateInputPlanNode::Symlink { target } => target,
                        _ => anyhow::bail!("non-symlink node has symlink target edge"),
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
                        CreateInputPlanEdge::IndirectResource { path: target_path },
                    ));
                }
            }
        }
    }

    for (node, target_node, weight) in indirect_resources {
        plan.graph.update_edge(node, target_node, weight);
    }

    Ok(())
}
