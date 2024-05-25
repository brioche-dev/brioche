use crate::{
    project::{ProjectHash, Projects},
    registry::PublishProjectResponse,
    Brioche,
};

pub async fn publish_project(
    brioche: &Brioche,
    projects: &Projects,
    project_hash: ProjectHash,
) -> anyhow::Result<PublishProjectResponse> {
    let project_listing = projects.export_listing(brioche, project_hash)?;
    let response = brioche
        .registry_client
        .publish_project(&project_listing)
        .await?;

    Ok(response)
}
