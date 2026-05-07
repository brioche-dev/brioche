#![cfg(feature = "weird-ui")]

use anyhow::Context as _;
use brioche_core::projects::ProjectSpecifier;
use futures::{StreamExt as _, TryStreamExt as _};
use weird_client::world::Node;

use crate::WeirdUiArgs;
use crate::utils::{ProjectSource, resolve_project_refs};

#[cfg(feature = "weird-ui")]
pub async fn launch_weird_ui(args: WeirdUiArgs) -> anyhow::Result<()> {
    let brioche = brioche_core::Brioche::new().await;

    let project_refs = resolve_project_refs(args.targets, args.project, args.registry, args.export);

    let specifiers = futures::stream::iter(project_refs.clone())
        .then(async |project_ref| {
            let specifier = match project_ref.source {
                ProjectSource::Local(path) => {
                    let path = brioche_core::path::canonicalize_system_path(&path).await?;
                    ProjectSpecifier::Path(path)
                }
                ProjectSource::Registry(registry) => {
                    anyhow::bail!("todo: registry project: {registry}");
                }
            };
            Ok(specifier)
        })
        .try_collect::<Vec<_>>()
        .await?;

    let projects =
        brioche_core::projects::load::load_projects(&brioche, specifiers.iter().cloned()).await?;

    let weird = weird_client::WeirdClient::builder()
        .app("brioche")
        .window_attr("width", 600)
        .window_attr("height", 400)
        .window_attr("replace", true)
        .connect()
        .context("failed to connect to Weird server")?;
    let highlight_projects = specifiers
        .iter()
        .map(|specifier| projects[specifier])
        .collect();
    let mut graph_options = brioche_core::projects::debug::ProjectGraphvizOptions {
        highlight_projects,
        ..Default::default()
    };

    loop {
        let project_graph = brioche_core::projects::debug::graphviz(&brioche, &graph_options).await;

        weird.render(vec![
            Node::text("Show modules:"),
            Node::element("Checkbox")
                .id("show_modules")
                .attr("value", graph_options.show_modules),
            Node::element("Graphviz")
                .attr("graph", project_graph)
                .attr("pan", true)
                .attr("zoom", true)
                .attr("maxZoom", 64)
                .attr("autoSize", true),
        ]);

        let Some(event) = weird.next_event().unwrap() else {
            break;
        };

        if event.is("show_modules", "change") {
            graph_options.show_modules = event.param("value").unwrap();
        }
    }

    Ok(())
}
