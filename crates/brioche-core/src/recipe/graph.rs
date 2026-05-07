use std::collections::BTreeSet;

use bstr::BString;
use petgraph::graph::NodeIndex;

use crate::{
    blob::BlobHash,
    hash::AnyHash,
    recipe::{ArchiveFormat, ArtifactKind, CompressionFormat},
};

pub(crate) type RecipeGraph =
    petgraph::stable_graph::StableDiGraph<RecipeGraphNode, RecipeGraphEdge>;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RecipeGraphNode;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RecipeGraphEdge;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecipeRef(pub(super) NodeIndex);
