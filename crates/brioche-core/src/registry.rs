use crate::{Brioche, projects::hash::ProjectHash};

const GET_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);

pub struct RegistryClientConfig {
    pub url: url::Url,
    pub retry: bool,
}

impl RegistryClientConfig {
    #[must_use]
    pub const fn new(url: url::Url) -> Self {
        Self { url, retry: true }
    }
}

#[derive(Debug, Clone)]
pub struct RegistryClient {
    reqwest: reqwest_middleware::ClientWithMiddleware,
    url: url::Url,
}

impl RegistryClient {
    #[must_use]
    pub fn new(config: RegistryClientConfig) -> Self {
        let reqwest = reqwest::Client::builder()
            .user_agent(crate::USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .expect("failed to build registry client");
        let mut reqwest = reqwest_middleware::ClientBuilder::new(reqwest);

        if config.retry {
            let retry_policy = reqwest_retry::policies::ExponentialBackoff::builder()
                .retry_bounds(
                    std::time::Duration::from_millis(500),
                    std::time::Duration::from_secs(3),
                )
                .build_with_max_retries(5);
            let retry_middleware =
                reqwest_retry::RetryTransientMiddleware::new_with_policy(retry_policy);
            reqwest = reqwest.with(retry_middleware);
        }

        let reqwest = reqwest.build();

        Self {
            reqwest,
            url: config.url,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest_middleware::RequestBuilder {
        let endpoint_url = self.url.join(path).unwrap_or_else(|error| {
            panic!(
                "failed to build registry URL with base URL '{}' and path '{path}': {error}",
                self.url
            );
        });
        self.reqwest
            .request(method, endpoint_url)
            .query(&[("brioche", env!("CARGO_PKG_VERSION"))])
    }
}

pub async fn get_project_tag(
    brioche: &Brioche,
    project_name: &str,
    tag: &str,
) -> Result<Option<GetProjectTagResponse>, RegistryError> {
    let project_name_component = urlencoding::Encoded::new(project_name);
    let tag_component = urlencoding::Encoded::new(tag);
    let response = brioche
        .registry_client
        .request(
            reqwest::Method::GET,
            &format!("v0/project-tags/{project_name_component}/{tag_component}"),
        )
        .timeout(GET_TIMEOUT)
        .send()
        .await?;

    if response.status().as_u16() == 404 {
        Ok(None)
    } else {
        let response_body = response.error_for_status()?.json().await?;
        Ok(Some(response_body))
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetProjectTagResponse {
    pub project_hash: ProjectHash,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RegistryError {
    #[error("{error_message}")]
    ReqwestError { error_message: String },

    #[error("{error_message}")]
    ReqwestMiddlewareError { error_message: String },
}

impl From<reqwest::Error> for RegistryError {
    fn from(value: reqwest::Error) -> Self {
        Self::ReqwestError {
            error_message: value.to_string(),
        }
    }
}

impl From<reqwest_middleware::Error> for RegistryError {
    fn from(value: reqwest_middleware::Error) -> Self {
        match value {
            reqwest_middleware::Error::Middleware(error) => Self::ReqwestMiddlewareError {
                error_message: error.to_string(),
            },
            reqwest_middleware::Error::Reqwest(error) => Self::from(error),
        }
    }
}
