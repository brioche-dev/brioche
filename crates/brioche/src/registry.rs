use anyhow::Context as _;

use crate::{
    blob::BlobId,
    project::{Project, ProjectHash, ProjectListing},
};

#[derive(Clone)]
pub enum RegistryClient {
    Enabled {
        client: reqwest::Client,
        url: url::Url,
        auth: RegistryAuthentication,
    },
    Disabled,
}

impl RegistryClient {
    pub fn new(url: url::Url, auth: RegistryAuthentication) -> Self {
        Self::Enabled {
            client: reqwest::Client::new(),
            url,
            auth,
        }
    }

    pub fn disabled() -> Self {
        Self::Disabled
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        let Self::Enabled { client, url, auth } = self else {
            return Err(anyhow::anyhow!("registry client is disabled"));
        };
        let endpoint_url = url.join(path).context("failed to construct registry URL")?;
        let request = client.request(method, endpoint_url);
        let request = match auth {
            RegistryAuthentication::Anonymous => request,
            RegistryAuthentication::Admin { password } => {
                request.basic_auth("admin", Some(password))
            }
        };
        Ok(request)
    }

    pub async fn get_blob(&self, blob_id: BlobId) -> anyhow::Result<Vec<u8>> {
        let file_id_component = urlencoding::Encoded::new(blob_id.to_string());
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("v0/blobs/{file_id_component}"),
            )?
            .send()
            .await?;
        let response_body = response.error_for_status()?.bytes().await?;
        let response_body = response_body.to_vec();

        blob_id
            .validate_matches(&response_body)
            .context("blob hash did not match")?;

        Ok(response_body)
    }

    pub async fn get_project_tag(
        &self,
        project_name: &str,
        tag: &str,
    ) -> anyhow::Result<GetProjectTagResponse> {
        let project_name_component = urlencoding::Encoded::new(project_name);
        let tag_component = urlencoding::Encoded::new(tag);
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("v0/project-tags/{project_name_component}/{tag_component}"),
            )?
            .send()
            .await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }

    pub async fn get_project(&self, project_hash: ProjectHash) -> anyhow::Result<Project> {
        let project_hash_component = urlencoding::Encoded::new(project_hash.to_string());
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("v0/projects/{project_hash_component}"),
            )?
            .send()
            .await?;
        let project = response.error_for_status()?.json().await?;

        project_hash.validate_matches(&project)?;

        Ok(project)
    }

    pub async fn publish_project(
        &self,
        project: &ProjectListing,
    ) -> anyhow::Result<PublishProjectResponse> {
        let response = self
            .request(reqwest::Method::POST, "v0/projects")?
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

impl PublishProjectResponse {
    pub fn is_no_op(&self) -> bool {
        self.new_files == 0 && self.new_projects == 0 && self.tags.is_empty()
    }
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