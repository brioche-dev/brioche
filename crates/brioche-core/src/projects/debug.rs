use std::{collections::HashSet, io::Write as _};

use crate::{
    Brioche,
    projects::{ModuleRef, ProjectEdge, ProjectNode, ProjectRef, ProjectSpecifier, WorkspaceRef},
    script::specifier::ImportSpecifier,
};

#[derive(Default)]
pub struct ProjectGraphvizOptions {
    pub show_modules: bool,
    pub highlight_projects: HashSet<ProjectRef>,
}

pub async fn graphviz(brioche: &Brioche, options: &ProjectGraphvizOptions) -> String {
    let projects = brioche.projects.read().await;

    let mut graphviz = Vec::<u8>::new();
    writeln!(&mut graphviz, "digraph {{").unwrap();

    writeln!(&mut graphviz, "graph [concentrate=true]").unwrap();

    for node_id in projects.graph.node_indices() {
        let node_idx = node_id.index();
        match projects.graph[node_id] {
            ProjectNode::Workspace => {
                let workspace = &projects.workspaces[&WorkspaceRef(node_id)];
                match workspace {
                    Ok(workspace) => {
                        let path = workspace.root.to_system_path();
                        let name = path
                            .as_ref()
                            .ok()
                            .and_then(|path| path.file_name())
                            .and_then(|name| name.to_str())
                            .unwrap_or("<unknown>");
                        writeln!(&mut graphviz, r#"{node_idx}[label="workspace:{name}"]"#).unwrap();
                    }
                    Err(error) => {
                        writeln!(
                            &mut graphviz,
                            r#"{node_idx}[label="workspace:<err>", tooltip="{error}"]"#
                        )
                        .unwrap();
                    }
                }
            }
            ProjectNode::Project => {
                let project = &projects.projects[&ProjectRef(node_id)];
                let label = if let Some(name) = &project.definition.name
                    && let Some(version) = &project.definition.version
                {
                    format!("{name}@{version}")
                } else if let Some(name) = &project.definition.name {
                    name.clone()
                } else {
                    match &project.specifier {
                        ProjectSpecifier::Path(path) => path
                            .to_system_path()
                            .ok()
                            .and_then(|path| {
                                path.file_name()
                                    .and_then(std::ffi::OsStr::to_str)
                                    .map(ToString::to_string)
                            })
                            .unwrap_or_else(|| "<unknown>".to_string()),
                        ProjectSpecifier::Hash(hash) => format!("hash:{hash}"),
                    }
                };

                if options.highlight_projects.contains(&ProjectRef(node_id)) {
                    writeln!(
                        &mut graphviz,
                        r#"{node_idx}[shape=box, label="{label}", color=darkgreen, style=filled, fillcolor=green, fontcolor=white]"#
                    )
                    .unwrap();
                } else {
                    writeln!(&mut graphviz, r#"{node_idx}[shape=box, label="{label}"]"#).unwrap();
                }
            }
            ProjectNode::Module => {
                if options.show_modules {
                    let module = &projects.modules[&ModuleRef(node_id)];
                    writeln!(
                        &mut graphviz,
                        r#"{node_idx}[shape = box, label = "{}", color = gray, fontcolor = gray]"#,
                        module.subpath
                    )
                    .unwrap();
                }
            }
        }
    }

    for edge_id in projects.graph.edge_indices() {
        let Some((source_id, target_id)) = projects.graph.edge_endpoints(edge_id) else {
            continue;
        };
        let edge = &projects.graph[edge_id];
        let source = &projects.graph[source_id];
        let target = &projects.graph[target_id];

        if !options.show_modules
            && (matches!(source, ProjectNode::Module) || matches!(target, ProjectNode::Module))
        {
            // Node excluded, so skip edge
        } else if matches!(edge, ProjectEdge::ModuleImport(ImportSpecifier::Local(_))) {
            writeln!(
                &mut graphviz,
                "{} -> {}[color=gray]",
                source_id.index(),
                target_id.index()
            )
            .unwrap();
        } else {
            writeln!(
                &mut graphviz,
                "{} -> {}",
                source_id.index(),
                target_id.index()
            )
            .unwrap();
        }
    }

    writeln!(&mut graphviz, "}}").unwrap();

    String::from_utf8(graphviz).unwrap()
}
