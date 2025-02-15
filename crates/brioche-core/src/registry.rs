use std::{
    collections::{HashMap, HashSet},
    env,
    sync::Arc,
};

use anyhow::Context as _;
use futures::{FutureExt as _, StreamExt as _, TryStreamExt as _};
use tokio::io::AsyncReadExt as _;

use crate::{
    blob::BlobHash,
    project::{Project, ProjectHash},
    recipe::{Artifact, Recipe, RecipeHash},
    Brioche,
};

const GET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Clone)]
pub enum RegistryClient {
    Enabled {
        client: reqwest_middleware::ClientWithMiddleware,
        url: url::Url,
        auth: RegistryAuthentication,
    },
    Disabled,
}

impl RegistryClient {
    pub fn new(url: url::Url, auth: RegistryAuthentication) -> Self {
        let retry_policy = reqwest_retry::policies::ExponentialBackoff::builder()
            .retry_bounds(
                std::time::Duration::from_millis(500),
                std::time::Duration::from_millis(3000),
            )
            .build_with_max_retries(5);
        let retry_middleware =
            reqwest_retry::RetryTransientMiddleware::new_with_policy(retry_policy);

        let client = reqwest::Client::builder()
            .user_agent(crate::USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .expect("failed to build reqwest client");
        let client = reqwest_middleware::ClientBuilder::new(client)
            .with(retry_middleware)
            .build();

        Self::new_with_client(client, url, auth)
    }

    pub fn new_with_client(
        client: reqwest_middleware::ClientWithMiddleware,
        url: url::Url,
        auth: RegistryAuthentication,
    ) -> Self {
        Self::Enabled { client, url, auth }
    }

    pub fn disabled() -> Self {
        Self::Disabled
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> anyhow::Result<reqwest_middleware::RequestBuilder> {
        let Self::Enabled { client, url, auth } = self else {
            return Err(anyhow::anyhow!("registry client is disabled"));
        };
        let endpoint_url = url.join(path).context("failed to construct registry URL")?;
        let request = client
            .request(method, endpoint_url)
            .query(&[("brioche", env!("CARGO_PKG_VERSION"))]);
        let request = match auth {
            RegistryAuthentication::Anonymous => request,
            RegistryAuthentication::Admin { password } => {
                request.basic_auth("admin", Some(password))
            }
        };
        Ok(request)
    }

    pub async fn get_blob(&self, blob_hash: BlobHash) -> anyhow::Result<Vec<u8>> {
        // No timeout for blobs, since they can take a while to download
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

    pub async fn send_blob(&self, blob_hash: BlobHash, content: Vec<u8>) -> anyhow::Result<()> {
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
            .timeout(GET_TIMEOUT)
            .send()
            .await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }

    pub async fn create_project_tags(
        &self,
        project_tags: &CreateProjectTagsRequest,
    ) -> anyhow::Result<CreateProjectTagsResponse> {
        let response = self
            .request(reqwest::Method::POST, "v0/project-tags")?
            .json(project_tags)
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
            .timeout(GET_TIMEOUT)
            .send()
            .await?;
        let project = response.error_for_status()?.json().await?;

        project_hash.validate_matches(&project)?;

        Ok(project)
    }

    pub async fn create_projects(
        &self,
        projects: &HashMap<ProjectHash, Arc<Project>>,
    ) -> anyhow::Result<u64> {
        let response = self
            .request(reqwest::Method::POST, "v0/projects")?
            .json(projects)
            .send()
            .await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }

    pub async fn get_recipe(&self, recipe_hash: RecipeHash) -> anyhow::Result<Recipe> {
        let response = self
            .request(reqwest::Method::GET, &format!("v0/recipes/{recipe_hash}"))?
            .timeout(GET_TIMEOUT)
            .send()
            .await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }

    pub async fn create_recipe(&self, recipe: &Recipe) -> anyhow::Result<RecipeHash> {
        let recipe_hash = recipe.hash();

        let response = self
            .request(reqwest::Method::PUT, &format!("v0/recipes/{recipe_hash}"))?
            .json(recipe)
            .send()
            .await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }

    pub async fn create_recipes(&self, artifacts: &[Recipe]) -> anyhow::Result<()> {
        for chunk in artifacts.chunks(1000) {
            let request: HashMap<_, _> = chunk
                .iter()
                .map(|recipe| (recipe.hash(), recipe.clone()))
                .collect();
            self.request(reqwest::Method::POST, "v0/recipes")?
                .json(&request)
                .send()
                .await?
                .error_for_status()?;
        }

        Ok(())
    }

    pub async fn known_recipes(
        &self,
        recipe_hashes: &[RecipeHash],
    ) -> anyhow::Result<HashSet<RecipeHash>> {
        let mut all_known_recipes = HashSet::new();
        for chunk in recipe_hashes.chunks(1000) {
            let known_recipes: Vec<RecipeHash> = self
                .request(reqwest::Method::POST, "v0/known-recipes")?
                .timeout(GET_TIMEOUT)
                .json(&chunk)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            all_known_recipes.extend(known_recipes);
        }
        Ok(all_known_recipes)
    }

    pub async fn known_blobs(&self, blobs: &[BlobHash]) -> anyhow::Result<HashSet<BlobHash>> {
        let mut all_known_blobs = HashSet::new();
        for chunk in blobs.chunks(1000) {
            let known_blobs: Vec<BlobHash> = self
                .request(reqwest::Method::POST, "v0/known-blobs")?
                .timeout(GET_TIMEOUT)
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

    pub async fn known_bakes(
        &self,
        bakes: &[(RecipeHash, RecipeHash)],
    ) -> anyhow::Result<HashSet<(RecipeHash, RecipeHash)>> {
        let mut all_known_bakes = HashSet::new();
        for chunk in bakes.chunks(1000) {
            let known_bakes: Vec<(RecipeHash, RecipeHash)> = self
                .request(reqwest::Method::POST, "v0/known-bakes")?
                .timeout(GET_TIMEOUT)
                .json(&chunk)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            all_known_bakes.extend(known_bakes);
        }
        Ok(all_known_bakes)
    }

    pub async fn known_projects(
        &self,
        project_hashes: &[ProjectHash],
    ) -> anyhow::Result<HashSet<ProjectHash>> {
        let mut all_known_projects = HashSet::new();
        for chunk in project_hashes.chunks(1000) {
            let known_projects: Vec<ProjectHash> = self
                .request(reqwest::Method::POST, "v0/known-projects")?
                .timeout(GET_TIMEOUT)
                .json(&chunk)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            all_known_projects.extend(known_projects);
        }
        Ok(all_known_projects)
    }

    pub async fn create_bake(
        &self,
        input_hash: RecipeHash,
        output_hash: RecipeHash,
    ) -> anyhow::Result<CreateBakeResponse> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("v0/recipes/{input_hash}/bake"),
            )?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&CreateBakeRequest { output_hash })
            .send()
            .await?;

        let response_body = response.error_for_status()?.json().await?;

        Ok(response_body)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_bake(&self, input_hash: RecipeHash) -> anyhow::Result<GetBakeResponse> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("v0/recipes/{input_hash}/bake"),
            )?
            .timeout(GET_TIMEOUT)
            .send()
            .await?;
        let response_body = response.error_for_status()?.json().await?;
        Ok(response_body)
    }
}

#[tracing::instrument(skip(brioche, response))]
pub async fn fetch_bake_references(
    brioche: Brioche,
    response: GetBakeResponse,
) -> anyhow::Result<()> {
    let unknown_blobs_fut = futures::stream::iter(response.referenced_blobs)
        .filter({
            let brioche = brioche.clone();
            move |&blob_hash| {
                let brioche = brioche.clone();
                async move {
                    let blob_path = super::blob::local_blob_path(&brioche, blob_hash);
                    !matches!(tokio::fs::try_exists(&blob_path).await, Ok(true))
                }
            }
        })
        .collect::<Vec<_>>()
        .map(anyhow::Ok);
    let known_recipes_fut =
        crate::references::local_recipes(&brioche, response.referenced_recipes.clone());
    let (unknown_blobs, known_recipes) = tokio::try_join!(unknown_blobs_fut, known_recipes_fut)?;
    let unknown_recipes = response
        .referenced_recipes
        .difference(&known_recipes)
        .copied()
        .collect::<Vec<_>>();

    // Short-circuit if we have nothing to fetch
    if unknown_blobs.is_empty() && unknown_recipes.is_empty() {
        return Ok(());
    }

    let fetch_blobs_fut = futures::stream::iter(unknown_blobs)
        .map(Ok)
        .try_for_each_concurrent(25, |blob| {
            let brioche = brioche.clone();
            async move {
                let mut permit = crate::blob::get_save_blob_permit().await?;
                super::blob::blob_path(&brioche, &mut permit, blob).await?;
                drop(permit);

                anyhow::Ok(())
            }
        });

    let new_recipes = Arc::new(tokio::sync::Mutex::new(vec![]));
    let fetch_recipes_fut = futures::stream::iter(unknown_recipes)
        .map(Ok)
        .try_for_each_concurrent(25, |recipe| {
            let brioche = brioche.clone();
            let new_recipes = new_recipes.clone();
            async move {
                let recipe = brioche.registry_client.get_recipe(recipe).await;
                if let Ok(recipe) = recipe {
                    let mut new_recipes = new_recipes.lock().await;
                    new_recipes.push(recipe);
                }

                anyhow::Ok(())
            }
        });

    tokio::try_join!(fetch_blobs_fut, fetch_recipes_fut)?;

    let mut new_recipes = new_recipes.lock().await;
    let new_recipes = std::mem::take(&mut *new_recipes);

    crate::recipe::save_recipes(&brioche, new_recipes).await?;

    Ok(())
}

#[tracing::instrument(skip(brioche, recipes))]
pub async fn fetch_recipes_deep(
    brioche: &Brioche,
    recipes: HashSet<RecipeHash>,
) -> anyhow::Result<()> {
    let mut pending_recipes = recipes;
    let mut checked_recipes = HashSet::new();

    loop {
        let needed_recipes: HashSet<_> = pending_recipes
            .difference(&checked_recipes)
            .copied()
            .collect();
        let known_recipes =
            crate::references::local_recipes(brioche, needed_recipes.iter().copied()).await?;
        let unknown_recipes = needed_recipes
            .difference(&known_recipes)
            .copied()
            .collect::<Vec<_>>();

        // If we have no recipes to fetch, we're done
        if unknown_recipes.is_empty() {
            break;
        }

        let new_recipes = Arc::new(tokio::sync::Mutex::new(vec![]));
        futures::stream::iter(unknown_recipes)
            .map(Ok)
            .try_for_each_concurrent(25, |recipe| {
                let brioche = brioche.clone();
                let new_recipes = new_recipes.clone();
                async move {
                    let recipe = brioche.registry_client.get_recipe(recipe).await;
                    if let Ok(recipe) = recipe {
                        let mut new_recipes = new_recipes.lock().await;
                        new_recipes.push(recipe);
                    }

                    anyhow::Ok(())
                }
            })
            .await?;

        checked_recipes.extend(pending_recipes.iter().copied());

        let mut new_recipes = new_recipes.lock().await;
        let new_recipes = std::mem::take(&mut *new_recipes);

        for recipe in &new_recipes {
            let referenced_recipes = crate::references::referenced_recipes(recipe);
            pending_recipes.extend(referenced_recipes);
        }

        checked_recipes.extend(new_recipes.iter().map(|recipe| recipe.hash()));
        crate::recipe::save_recipes(brioche, new_recipes).await?;
    }

    Ok(())
}

#[tracing::instrument(skip(brioche, blobs))]
pub async fn fetch_blobs(brioche: Brioche, blobs: HashSet<BlobHash>) -> anyhow::Result<()> {
    // Find the blobs we don't have locally, which we'll need to fetch
    let unknown_blobs = tokio::task::spawn_blocking({
        let brioche = brioche.clone();
        move || {
            let mut unknown_blobs = blobs.clone();
            for blob_hash in &blobs {
                let blob_path = super::blob::local_blob_path(&brioche, *blob_hash);
                let exists = std::fs::exists(&blob_path)?;
                if exists {
                    unknown_blobs.remove(blob_hash);
                }
            }

            anyhow::Ok(unknown_blobs)
        }
    })
    .await??;

    // Short-circuit if we have nothing to fetch
    if unknown_blobs.is_empty() {
        return Ok(());
    }

    futures::stream::iter(unknown_blobs)
        .map(Ok)
        .try_for_each_concurrent(25, |blob| {
            let brioche = brioche.clone();
            async move {
                let mut permit = crate::blob::get_save_blob_permit().await?;
                super::blob::blob_path(&brioche, &mut permit, blob).await?;
                drop(permit);

                anyhow::Ok(())
            }
        })
        .await?;

    Ok(())
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
pub struct CreateProjectTagsRequest {
    pub tags: Vec<CreateProjectTagsRequestTag>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectTagsRequestTag {
    pub project_name: String,
    pub tag: String,
    pub project_hash: ProjectHash,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectTagsResponse {
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBakeResponse {
    pub output_hash: RecipeHash,
    pub output_artifact: Artifact,
    pub referenced_recipes: HashSet<RecipeHash>,
    pub referenced_blobs: HashSet<BlobHash>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBakeRequest {
    pub output_hash: RecipeHash,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBakeResponse {
    pub canonical_output_hash: RecipeHash,
}
