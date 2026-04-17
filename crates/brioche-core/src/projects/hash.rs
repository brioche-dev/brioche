use std::collections::{BTreeMap, HashMap, HashSet};

use petgraph::visit::EdgeRef as _;

use crate::{
    Brioche,
    path::RelativePath,
    projects::{ProjectDefinition, ProjectRef},
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ProjectHash(crate::hash::Blake3Hash);

impl std::fmt::Display for ProjectHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub async fn hash_project(
    brioche: &Brioche,
    project_ref: ProjectRef,
) -> Result<ProjectHash, std::convert::Infallible> {
    let projects = brioche.projects.read().await;

    // Create a copy of the graph, but keeping only project nodes that
    // are reachable from the project we're hashing
    let mut graph = projects.graph.clone();
    let mut dfs_space = petgraph::algo::DfsSpace::default();
    graph.retain_nodes(|graph, index| match &graph[index] {
        crate::projects::ProjectNode::Project => {
            petgraph::algo::has_path_connecting(&*graph, project_ref.0, index, Some(&mut dfs_space))
        }
        crate::projects::ProjectNode::Workspace | crate::projects::ProjectNode::Module => false,
    });

    // Group nodes by finding the strongly-connected components of the graph.
    // This effectively finds cyclic projects in the graph that we should
    // group together, and puts acyclic projects into a group of one element.
    // The result is additionally topographically sorted, so every project
    // naturally comes after all of its dependencies
    let node_groups = petgraph::algo::tarjan_scc(&graph);

    let mut project_hashes = HashMap::<ProjectRef, ProjectHash>::new();

    for group_nodes in node_groups {
        let group_nodes: HashSet<_> = group_nodes.into_iter().collect();

        if group_nodes.len() > 1 {
            unimplemented!("cyclic project");
        }

        let project_ref = group_nodes.iter().next().unwrap();
        let project_ref = ProjectRef(*project_ref);
        let project = &projects.projects[&project_ref];

        let dependencies = graph
            .edges(project_ref.0)
            .filter_map(|edge| match edge.weight() {
                crate::projects::ProjectEdge::ProjectDependency(dep_name) => {
                    let dep_hash = project_hashes[&ProjectRef(edge.target())];
                    Some((dep_name.clone(), DependencyRef::Project(dep_hash)))
                }
                crate::projects::ProjectEdge::ProjectWithinWorkspace
                | crate::projects::ProjectEdge::ProjectRootModule
                | crate::projects::ProjectEdge::ModuleImport(_) => None,
            })
            .collect();

        let modules = projects.modules_by_project[&project_ref]
            .iter()
            .map(|(path, module_ref)| {
                let module_ast = projects.modules[module_ref]
                    .ast
                    .as_ref()
                    .expect("todo: handle module error");
                let source_hash = blake3::hash(module_ast.source.as_bytes());
                (path.clone(), crate::hash::Blake3Hash::from(source_hash))
            })
            .collect();

        let mut statics = HashMap::new(); // TODO: statics

        let project = ContentAddressedProject {
            definition: project.definition.clone(),
            dependencies,
            modules,
            statics,
        };
        let project = ContentAddressedProjectEntry::Project(project);

        let mut hasher = blake3::Hasher::new();
        json_canon::to_writer(&mut hasher, &project).expect("todo: handle serialize error");
        let project_hash = ProjectHash(hasher.finalize().into());

        project_hashes.insert(project_ref, project_hash);
    }

    Ok(project_hashes[&project_ref])
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ContentAddressedProjectEntry {
    WorkspaceMember {
        workspace: WorkspaceHash,
        path: RelativePath,
    },
    #[serde(untagged)]
    Project(ContentAddressedProject),
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
struct WorkspaceHash(crate::hash::Blake3Hash);

impl std::fmt::Display for WorkspaceHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentAddressedProject {
    definition: ProjectDefinition,
    dependencies: HashMap<String, DependencyRef>,
    modules: HashMap<RelativePath, crate::hash::Blake3Hash>,
    #[serde_as(as = "HashMap<_, Vec<(_, _)>>")]
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    statics: HashMap<RelativePath, BTreeMap<StaticQuery, Option<StaticOutput>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum DependencyRef {
    WorkspaceMember {
        path: RelativePath,
    },
    #[serde(untagged)]
    Project(ProjectHash),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum StaticQuery {
    Include(StaticInclude),
    Glob { patterns: Vec<String> },
    Download { url: url::Url },
    GitRef(GitRefOptions),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "include")]
#[serde(rename_all = "snake_case")]
enum StaticInclude {
    File { path: String },
    Directory { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
struct GitRefOptions {
    pub repository: url::Url,

    #[serde(rename = "ref")]
    pub ref_: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum StaticOutput {
    RecipeHash(crate::recipe::RecipeHash),
    Kind(StaticOutputKind),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
enum StaticOutputKind {
    Download { hash: crate::hash::AnyHash },
    GitRef { commit: String },
}
