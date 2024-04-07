use anyhow::Context as _;

use crate::{
    project::{Project, ProjectHash, ProjectListing},
    vfs::FileId,
};

#[derive(Clone)]
pub struct RegistryClient {
    client: reqwest::Client,
    url: url::Url,
    auth: RegistryAuthentication,
}

impl RegistryClient {
    pub fn new(url: url::Url, auth: RegistryAuthentication) -> Self {
        Self {
            client: reqwest::Client::new(),
            url,
            auth,
        }
    }

    fn request(
        &self,
        method: reqwest::Method,
        url: impl reqwest::IntoUrl,
    ) -> reqwest::RequestBuilder {
        let request = self.client.request(method, url);
        match &self.auth {
            RegistryAuthentication::Anonymous => request,
            RegistryAuthentication::Admin { password } => {
                request.basic_auth("admin", Some(password))
            }
        }
    }

    pub async fn get_blob(&self, file_id: FileId) -> anyhow::Result<Vec<u8>> {
        let url = self
            .url
            .join("blobs/")?
            .join(&urlencoding::encode(&file_id.to_string()))?;
        let response = self.request(reqwest::Method::GET, url).send().await?;
        let response_body = response.error_for_status()?.bytes().await?;
        let response_body = response_body.to_vec();

        file_id
            .validate_matches(&response_body)
            .context("blob hash did not match")?;

        Ok(response_body)
    }

    pub async fn get_project_tag(
        &self,
        project_name: &str,
        tag: &str,
    ) -> anyhow::Result<GetProjectTagResponse> {
        let url = self
            .url
            .join("project-tags/")?
            .join(&format!("{}/", urlencoding::encode(project_name)))?
            .join(&urlencoding::encode(tag))?;
        let response = self.request(reqwest::Method::GET, url).send().await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }

    pub async fn get_project(&self, project_hash: ProjectHash) -> anyhow::Result<Project> {
        let url = self
            .url
            .join("projects/")?
            .join(&urlencoding::encode(&project_hash.to_string()))?;
        let response = self.request(reqwest::Method::GET, url).send().await?;
        let project = response.error_for_status()?.json().await?;

        project_hash.validate_matches(&project)?;

        Ok(project)
    }

    pub async fn publish_project(
        &self,
        project: &ProjectListing,
    ) -> anyhow::Result<PublishProjectResponse> {
        let url = self.url.join("projects")?;
        let response = self
            .request(reqwest::Method::POST, url)
            .json(project)
            .send()
            .await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }
}

#[derive(Clone)]
pub enum RegistryAuthentication {
    Anonymous,
    Admin { password: String },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetProjectTagResponse {
    pub project_hash: ProjectHash,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishProjectResponse {
    pub root_project: ProjectHash,
    pub new_files: u64,
    pub new_projects: u64,
    pub tags: Vec<UpdatedTag>,
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatedTag {
    pub name: String,
    pub tag: String,
    #[serde_as(as = "Option<serde_with::DisplayFromStr>")]
    pub previous_hash: Option<ProjectHash>,
}
