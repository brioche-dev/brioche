use anyhow::Context as _;

use crate::{
    project::{ProjectHash, Projects},
    references::ProjectReferences,
    registry::CreateProjectTagsResponse,
    Brioche,
};

pub async fn publish_project(
    brioche: &Brioche,
    projects: &Projects,
    project_hash: ProjectHash,
    verbose: bool,
) -> anyhow::Result<CreateProjectTagsResponse> {
    // Validate the project has a name to publish with
    let project = projects
        .project(project_hash)
        .context("project not found")?;
    let project_name = project.definition.name.clone().context("project must have a name to be published (does the root module have `export const project = { ... }`?")?;

    // Get all project references (project dependencies / blobs / recipes)
    let mut project_references = ProjectReferences::default();
    crate::references::project_references(
        brioche,
        projects,
        &mut project_references,
        [project_hash],
    )
    .await?;

    // Sync the project and all references
    crate::sync::legacy_sync_project_references(brioche, &project_references, verbose).await?;

    // Push new project tags ("latest" plus the version number)
    let project_tags = ["latest"]
        .into_iter()
        .chain(project.definition.version.as_deref());
    let project_tags = project_tags
        .map(|tag| crate::registry::CreateProjectTagsRequestTag {
            project_name: project_name.clone(),
            tag: tag.to_string(),
            project_hash,
        })
        .collect();
    let project_tags_request = crate::registry::CreateProjectTagsRequest { tags: project_tags };
    let response = brioche
        .registry_client
        .create_project_tags(&project_tags_request)
        .await?;

    Ok(response)
}
