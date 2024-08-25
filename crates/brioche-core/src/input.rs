use std::{
    collections::{BTreeMap, HashMap},
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

impl InputOptions<'_> {
    fn has_resource_dirs(&self) -> bool {
        self.resource_dir.is_some() || !self.input_resource_dirs.is_empty()
    }
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

            add_input_plan_indirect_resources(
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

            anyhow::Ok((plan, root_node))
        }
    })
    .await??;

    let mut nodes_to_artifacts = HashMap::<petgraph::graph::NodeIndex, WithMeta<Artifact>>::new();
    let planned_nodes = petgraph::algo::toposort(petgraph::visit::Reversed(&plan.graph), None)
        .map_err(|_| anyhow::anyhow!("cycle detected in input {:?}", options.input_path))?;

    for node_index in planned_nodes {
        let node = &plan.graph[node_index];
        let path = plan.nodes_to_paths[&node_index].to_owned();

        let artifact = match node {
            CreateInputPlanNode::File { is_executable } => {
                let content_blob = super::blob::save_blob_from_file(
                    brioche,
                    &mut super::blob::get_save_blob_permit().await?,
                    &path,
                    super::blob::SaveBlobOptions::default().remove_input(options.remove_input),
                    buffer,
                )
                .await?;

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
                    content_blob,
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

    let artifact = nodes_to_artifacts
        .remove(&root_node)
        .ok_or_else(|| anyhow::anyhow!("root node not found"))?;
    Ok(artifact)
}

#[async_recursion::async_recursion]
#[tracing::instrument(skip_all, err, fields(input_path = %options.input_path.display()))]
pub async fn create_input_inner_old(
    brioche: &Brioche,
    options: InputOptions<'async_recursion>,
    buffer: &mut Vec<u8>,
) -> anyhow::Result<WithMeta<Artifact>> {
    if let Some(saved) = options.saved_paths.get(options.input_path) {
        return Ok(WithMeta::new(saved.clone(), options.meta.clone()));
    }

    let metadata = tokio::fs::symlink_metadata(options.input_path)
        .await
        .with_context(|| {
            format!(
                "failed to get metadata for {}",
                options.input_path.display()
            )
        })?;

    let artifact = if metadata.is_file() {
        let resources = create_input_file_resources(
            brioche,
            InputOptions {
                input_path: options.input_path,
                remove_input: options.remove_input,
                resource_dir: options.resource_dir,
                input_resource_dirs: options.input_resource_dirs,
                saved_paths: options.saved_paths,
                meta: options.meta,
            },
            buffer,
        )
        .await?;

        let blob_hash = {
            let mut permit = super::blob::get_save_blob_permit().await?;
            super::blob::save_blob_from_file(
                brioche,
                &mut permit,
                options.input_path,
                super::blob::SaveBlobOptions::default().remove_input(options.remove_input),
                buffer,
            )
            .await
        }?;
        let permissions = metadata.permissions();
        let executable = is_executable(&permissions);

        Artifact::File(File {
            content_blob: blob_hash,
            executable,
            resources,
        })
    } else if metadata.is_dir() {
        let mut dir = tokio::fs::read_dir(options.input_path)
            .await
            .with_context(|| {
                format!("failed to read directory {}", options.input_path.display())
            })?;

        let mut result_dir_entries = BTreeMap::new();

        while let Some(entry) = dir.next_entry().await? {
            let entry_name = <Vec<u8> as bstr::ByteVec>::from_os_string(entry.file_name())
                .map_err(|_| {
                    anyhow::anyhow!(
                        "invalid file name {} in directory {}",
                        entry.file_name().to_string_lossy(),
                        options.input_path.display()
                    )
                })?;
            let entry_name = bstr::BString::from(entry_name);

            let result_entry = create_input_inner(
                brioche,
                InputOptions {
                    input_path: &entry.path(),
                    input_resource_dirs: options.input_resource_dirs,
                    resource_dir: options.resource_dir,
                    remove_input: options.remove_input,
                    saved_paths: options.saved_paths,
                    meta: options.meta,
                },
                buffer,
            )
            .await?;

            result_dir_entries.insert(entry_name, result_entry);
        }

        if options.remove_input {
            tokio::fs::remove_dir(options.input_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to remove directory at {}",
                        options.input_path.display()
                    )
                })?;
        }

        let result_dir = Directory::create(brioche, &result_dir_entries).await?;
        Artifact::Directory(result_dir)
    } else if metadata.is_symlink() {
        let target = tokio::fs::read_link(options.input_path)
            .await
            .with_context(|| {
                format!("failed to read symlink at {}", options.input_path.display())
            })?;
        let target = <Vec<u8> as bstr::ByteVec>::from_path_buf(target).map_err(|_| {
            anyhow::anyhow!("invalid symlink target at {}", options.input_path.display())
        })?;
        let target = bstr::BString::from(target);

        if options.remove_input {
            tokio::fs::remove_file(options.input_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to remove symlink at {}",
                        options.input_path.display()
                    )
                })?;
        }

        Artifact::Symlink { target }
    } else {
        anyhow::bail!(
            "unsupported input file type at {}",
            options.input_path.display()
        );
    };

    options
        .saved_paths
        .insert(options.input_path.to_owned(), artifact.clone());
    Ok(WithMeta::new(artifact, options.meta.clone()))
}

async fn create_input_file_resources(
    brioche: &Brioche,
    options: InputOptions<'_>,
    buffer: &mut Vec<u8>,
) -> anyhow::Result<Directory> {
    if !options.has_resource_dirs() {
        return Ok(Directory::default());
    }

    let pack = tokio::task::spawn_blocking({
        let input_path = options.input_path.to_owned();
        move || {
            let input_file = std::fs::File::open(&input_path)
                .with_context(|| format!("failed to open file {}", input_path.display()))?;
            let extracted = brioche_pack::extract_pack(input_file).ok();
            let pack = extracted.map(|extracted| extracted.pack);
            anyhow::Ok(pack)
        }
    })
    .await?
    .context("failed to extract resource pack")?;

    let pack_paths = pack.iter().flat_map(|pack| pack.paths());

    let mut pack_paths: Vec<_> = pack_paths.collect();
    let mut resources = Directory::default();
    while let Some(pack_path) = pack_paths.pop() {
        let path = pack_path.to_path().context("invalid resource path")?;
        let resource =
            find_resource(options.resource_dir, options.input_resource_dirs, path).await?;

        let Some(resource) = resource else {
            anyhow::bail!(
                "resource for input {} not found: {}",
                options.input_path.display(),
                path.display()
            );
        };

        let resource_artifact = create_input_inner(
            brioche,
            InputOptions {
                input_path: &resource.path,
                remove_input: false,
                resource_dir: options.resource_dir,
                input_resource_dirs: options.input_resource_dirs,
                saved_paths: options.saved_paths,
                meta: options.meta,
            },
            buffer,
        )
        .await?;

        tracing::debug!(resource = %resource.path.display(), "found resource");
        resources
            .insert(brioche, &pack_path, Some(resource_artifact))
            .await?;

        // Add the symlink's target to the resource dir as well
        if resource.metadata.is_symlink() {
            let target = match tokio::fs::canonicalize(&resource.path).await {
                Ok(target) => target,
                Err(err) => {
                    tracing::warn!(resource = %resource.path.display(), "invalid resource symlink: {err}");
                    continue;
                }
            };
            let canonical_resource_dir = tokio::fs::canonicalize(&resource.resource_dir).await;
            let canonical_resource_dir = match canonical_resource_dir {
                Ok(target) => target,
                Err(err) => {
                    tracing::warn!(resource_dir = %resource.resource_dir.display(), "failed to canonicalize resource dir: {err}");
                    continue;
                }
            };
            let target = match target.strip_prefix(&canonical_resource_dir) {
                Ok(target) => target,
                Err(err) => {
                    tracing::warn!(resource = %resource.path.display(), "resource symlink target not under resource dir: {err}");
                    continue;
                }
            };

            tracing::debug!(target = %target.display(), "queueing symlink resource target");

            let target = Vec::<u8>::from_path_buf(target.to_owned()).map_err(|_| {
                anyhow::anyhow!("invalid symlink target at {}", resource.path.display())
            })?;

            pack_paths.push(target.into());
        } else if resource.metadata.is_dir() {
            let mut dir = tokio::fs::read_dir(&resource.path)
                .await
                .with_context(|| format!("failed to read directory {}", resource.path.display()))?;

            tracing::debug!(resource_path = %resource.path.display(), "queueing directory entry resource");

            while let Some(entry) = dir.next_entry().await.transpose() {
                let entry = entry.context("failed to read directory entry")?;
                let entry_path = path.join(entry.file_name());
                let entry_path =
                    <Vec<u8> as bstr::ByteVec>::from_path_buf(entry_path).map_err(|_| {
                        anyhow::anyhow!(
                            "invalid entry {} in directory {}",
                            entry.file_name().to_string_lossy(),
                            resource.path.display()
                        )
                    })?;

                pack_paths.push(entry_path.into());
            }
        }
    }

    Ok(resources)
}

pub struct FoundResource<'a> {
    pub path: PathBuf,
    pub metadata: std::fs::Metadata,
    pub resource_dir: &'a Path,
}

async fn find_resource<'a>(
    resource_dir: Option<&'a Path>,
    input_resource_dirs: &'a [PathBuf],
    subpath: &Path,
) -> anyhow::Result<Option<FoundResource<'a>>> {
    let resource_dirs = resource_dir
        .into_iter()
        .chain(input_resource_dirs.iter().map(|dir| &**dir));

    for resource_dir in resource_dirs {
        let resource_path = resource_dir.join(subpath);
        let resource_metadata = tokio::fs::symlink_metadata(&resource_path).await;
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

fn add_input_plan_indirect_resources(
    options: InputOptions,
    plan: &mut CreateInputPlan,
) -> anyhow::Result<()> {
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

    let mut indirect_resources = vec![];

    while let Some((node, resource_path, resource_node)) = resource_paths.pop() {
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
