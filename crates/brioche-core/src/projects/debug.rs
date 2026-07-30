use std::{collections::HashSet, io::Write as _};

use petgraph::visit::EdgeRef as _;

use crate::{
    Brioche,
    projects::{
        ModuleRef, ProjectEdge, ProjectNode, ProjectRef, ProjectSpecifier, Projects, StaticRef,
        WorkspaceRef,
    },
    script::specifier::ImportSpecifier,
};

#[derive(Default)]
pub struct ProjectGraphvizOptions {
    pub show_modules: bool,
    pub show_statics: bool,
    pub highlight_projects: HashSet<ProjectRef>,
}

pub async fn graphviz(brioche: &Brioche, options: &ProjectGraphvizOptions) -> String {
    let projects = brioche.projects.read().await;
    graphviz_inner(&projects, &projects.graph, options)
}

pub(crate) fn graphviz_inner(
    projects: &Projects,
    graph: &super::ProjectGraph,
    options: &ProjectGraphvizOptions,
) -> String {
    let mut graphviz = Vec::<u8>::new();
    writeln!(&mut graphviz, "digraph {{").unwrap();

    writeln!(&mut graphviz, "graph [concentrate=true]").unwrap();

    for node_id in graph.node_indices() {
        let node_idx = node_id.index();
        match graph[node_id] {
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
            ProjectNode::Static => {
                if options.show_modules && options.show_statics {
                    let static_ = &projects.statics[&StaticRef(node_id)];
                    let module_path = graph
                        .edges_directed(node_id, petgraph::Direction::Incoming)
                        .find_map(|edge| {
                            let module_ref = match edge.weight() {
                                ProjectEdge::ModuleStatic(_) => ModuleRef(edge.source()),
                                _ => {
                                    return None;
                                }
                            };

                            let module = &projects.modules[&module_ref];
                            let project_path = &projects.local_project_paths[&module.project];
                            let (_, module_subpath) = &projects.project_by_module[&module_ref];
                            project_path.join_subpath(module_subpath.clone()).ok()
                        });

                    let label = match static_ {
                        super::Static::IncludeFile(path) => {
                            format!("file {path}")
                        }
                        super::Static::IncludeDirectory(path) => {
                            format!("dir {path}")
                        }
                        super::Static::Glob { patterns } => {
                            if let [pattern] = &patterns[..] {
                                format!("glob {pattern}")
                            } else {
                                "glob ...".to_string()
                            }
                        }
                        super::Static::Download { url, hash: _ } => {
                            format!("download {url}")
                        }
                        super::Static::GitRef {
                            repository,
                            ref_,
                            commit: _,
                        } => format!("git {repository} {ref_}"),
                    };
                    let tooltip = match static_ {
                        super::Static::IncludeFile(path) => module_path.map(|module_path| {
                            let path = module_path.join(path.clone());
                            format!("file {path}")
                        }),
                        super::Static::IncludeDirectory(path) => module_path.map(|module_path| {
                            let path = module_path.join(path.clone());
                            format!("dir {path}")
                        }),
                        super::Static::Glob { patterns } => {
                            if let [pattern] = &patterns[..] {
                                Some(format!("glob {pattern}"))
                            } else {
                                Some(format!("glob ...({})", patterns.len()))
                            }
                        }
                        super::Static::Download { .. } | super::Static::GitRef { .. } => None,
                    };
                    let tooltip = tooltip.as_deref().unwrap_or(&label);
                    writeln!(
                        &mut graphviz,
                        r#"{node_idx}[shape = box, label = "{label}", tooltip = "{tooltip}", color = gray, fontcolor = gray]"#
                    )
                    .unwrap();
                }
            }
            ProjectNode::UnresolvedStatic => {
                if options.show_modules && options.show_statics {
                    let static_ = &projects.unresolved_statics[&StaticRef(node_id)];

                    let label = match static_ {
                        super::UnresolvedStatic::Download { url } => {
                            format!("download {url}")
                        }
                        super::UnresolvedStatic::GitRef { repository, ref_ } => {
                            format!("git {repository} {ref_}")
                        }
                    };
                    writeln!(
                        &mut graphviz,
                        r#"{node_idx}[shape = box, label = "?{label}", tooltip = "(unresolved) {label}", color = gray, fontcolor = gray]"#
                    )
                    .unwrap();
                }
            }
        }
    }

    for edge_id in graph.edge_indices() {
        let Some((source_id, target_id)) = graph.edge_endpoints(edge_id) else {
            continue;
        };
        let edge = &graph[edge_id];
        let source = &graph[source_id];
        let target = &graph[target_id];

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
