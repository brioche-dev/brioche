use anyhow::Context as _;

use crate::{
    Brioche,
    project::{ProjectHash, Projects},
    recipe::Artifact,
    references::ProjectReferences,
    registry::CreateProjectTagsResponse,
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

    // Check if the project includes any cyclic dependencies
    let includes_cycles = projects.project_includes_cycles(project_hash)?;

    // Get all project references (project dependencies / blobs / recipes).
    // This is only used for legacy publishing, and is skipped if the
    // project includes cyclic dependencies
    let project_references = if includes_cycles {
        None
    } else {
        let mut project_references = ProjectReferences::default();
        crate::references::project_references(
            brioche,
            projects,
            &mut project_references,
            [project_hash],
        )
        .await?;
        Some(project_references)
    };

    // Create an artifact for the project and save it to the cache
    let project_artifact =
        crate::project::artifact::create_artifact_with_projects(brioche, projects, &[project_hash])
            .await?;
    let project_artifact = Artifact::Directory(project_artifact);
    let project_artifact_hash = project_artifact.hash();
    crate::cache::save_artifact(brioche, project_artifact).await?;
    crate::cache::save_project_artifact_hash(brioche, project_hash, project_artifact_hash).await?;

    // Sync the project and all references to the registry (legacy). This
    // is only supported if the project doesn't include cyclic dependencies.
    if let Some(project_references) = &project_references {
        crate::sync::legacy_sync_project_references(brioche, project_references, verbose).await?;
    } else {
        tracing::info!("project includes cyclic dependencies, not publishing to legacy registry");
    }

    // Push new project tags ("latest" plus the version number)
    let project_tags = std::iter::once("latest").chain(project.definition.version.as_deref());
    let project_tags = project_tags
        .map(|tag| crate::registry::CreateProjectTagsRequestTag {
            project_name: project_name.clone(),
            tag: tag.to_string(),
            project_hash,
        })
        .collect();
    let project_tags_request = crate::registry::CreateProjectTagsRequest {
        tags: project_tags,
        includes_cycles: Some(includes_cycles),
    };
    let response = brioche
        .registry_client
        .create_project_tags(&project_tags_request)
        .await?;

    Ok(response)
}
