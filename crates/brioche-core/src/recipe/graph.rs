use petgraph::graph::NodeIndex;

pub type RecipeGraph = petgraph::stable_graph::StableDiGraph<RecipeGraphNode, RecipeGraphEdge>;

#[derive(Debug, Clone, Copy)]
pub struct RecipeGraphNode;

#[derive(Debug, Clone, Copy)]
pub struct RecipeGraphEdge;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecipeRef(pub(super) NodeIndex);
