use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use futures::TryStreamExt as _;
use tokio::io::AsyncReadExt as _;

use crate::{
    blob::BlobHash,
    project::{Project, ProjectHash, ProjectListing},
    recipe::{Artifact, Recipe, RecipeHash},
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

    pub async fn get_blob(&self, blob_hash: BlobHash) -> anyhow::Result<Vec<u8>> {
        let response = self
            .request(reqwest::Method::GET, &format!("v0/blobs/{blob_hash}.zst"))?
            .send()
            .await?
            .error_for_status()?;

        let response_stream = response.bytes_stream().map_err(std::io::Error::other);
        let response_reader = tokio_util::io::StreamReader::new(response_stream);
        let mut response_reader =
            async_compression::tokio::bufread::ZstdDecoder::new(response_reader);

        let mut response_body = vec![];
        response_reader.read_to_end(&mut response_body).await?;

        blob_hash
            .validate_matches(&response_body)
            .context("blob hash did not match")?;

        Ok(response_body)
    }

    pub async fn send_blob(
        &self,
        blob_hash: BlobHash,
        content: impl Into<reqwest::Body>,
    ) -> anyhow::Result<()> {
        let path = format!("v0/blobs/{blob_hash}");

        self.request(reqwest::Method::PUT, &path)?
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(content)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
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

    pub async fn create_artifact(&self, artifact: &Recipe) -> anyhow::Result<RecipeHash> {
        let artifact_hash = artifact.hash();

        let response = self
            .request(
                reqwest::Method::PUT,
                &format!("v0/artifacts/{artifact_hash}"),
            )?
            .json(artifact)
            .send()
            .await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }

    pub async fn create_artifacts(&self, artifacts: &[Recipe]) -> anyhow::Result<()> {
        for chunk in artifacts.chunks(1000) {
            let request: HashMap<_, _> = chunk
                .iter()
                .map(|artifact| (artifact.hash(), artifact.clone()))
                .collect();
            self.request(reqwest::Method::POST, "v0/artifacts")?
                .json(&request)
                .send()
                .await?
                .error_for_status()?;
        }

        Ok(())
    }

    pub async fn known_artifacts(
        &self,
        artifact_hashes: &[RecipeHash],
    ) -> anyhow::Result<HashSet<RecipeHash>> {
        let mut all_known_artifacts = HashSet::new();
        for chunk in artifact_hashes.chunks(1000) {
            let known_artifacts: Vec<RecipeHash> = self
                .request(reqwest::Method::POST, "v0/known-artifacts")?
                .json(&chunk)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            all_known_artifacts.extend(known_artifacts);
        }
        Ok(all_known_artifacts)
    }

    pub async fn known_blobs(&self, blobs: &[BlobHash]) -> anyhow::Result<HashSet<BlobHash>> {
        let mut all_known_blobs = HashSet::new();
        for chunk in blobs.chunks(1000) {
            let known_blobs: Vec<BlobHash> = self
                .request(reqwest::Method::POST, "v0/known-blobs")?
                .json(&chunk)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            all_known_blobs.extend(known_blobs);
        }
        Ok(all_known_blobs)
    }

    pub async fn known_resolves(
        &self,
        resolves: &[(RecipeHash, RecipeHash)],
    ) -> anyhow::Result<HashSet<(RecipeHash, RecipeHash)>> {
        let mut all_known_resolves = HashSet::new();
        for chunk in resolves.chunks(1000) {
            let known_resolves: Vec<(RecipeHash, RecipeHash)> = self
                .request(reqwest::Method::POST, "v0/known-resolves")?
                .json(&chunk)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            all_known_resolves.extend(known_resolves);
        }
        Ok(all_known_resolves)
    }

    pub async fn create_resolve(
        &self,
        input_hash: RecipeHash,
        output_hash: RecipeHash,
    ) -> anyhow::Result<CreateResolveResponse> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("v0/artifacts/{input_hash}/resolve"),
            )?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&CreateResolveRequest { output_hash })
            .send()
            .await?;

        let response_body = response.error_for_status()?.json().await?;

        Ok(response_body)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_resolve(&self, input_hash: RecipeHash) -> anyhow::Result<GetResolveResponse> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("v0/artifacts/{input_hash}/resolve"),
            )?
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

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetResolveResponse {
    pub output_hash: RecipeHash,
    pub output_artifact: Artifact,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResolveRequest {
    pub output_hash: RecipeHash,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResolveResponse {
    pub canonical_output_hash: RecipeHash,
}
