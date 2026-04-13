use std::sync::Arc;

use tokio::sync::RwLock;

mod encoding;
mod hash;
pub mod path;
pub mod projects;
mod recipe;
pub mod registry;
mod script;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!("brioche/", env!("CARGO_PKG_VERSION"));

static DEFAULT_REGISTRY_URL: std::sync::LazyLock<url::Url> =
    std::sync::LazyLock::new(|| "https://registry.brioche.dev/".parse().unwrap());

#[derive(Clone)]
pub struct Brioche {
    projects: Arc<RwLock<projects::Projects>>,
    registry: registry::RegistryClient,
}

impl Brioche {
    #[must_use]
    pub const fn builder() -> BriocheBuilder {
        BriocheBuilder { registry_url: None }
    }

    #[must_use]
    #[expect(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::builder().build()
    }
}

pub struct BriocheBuilder {
    registry_url: Option<url::Url>,
}

impl BriocheBuilder {
    #[must_use]
    pub fn registry_url(mut self, registry_url: url::Url) -> Self {
        self.registry_url = Some(registry_url);
        self
    }

    #[must_use]
    pub fn build(self) -> Brioche {
        let registry_url = self
            .registry_url
            .unwrap_or_else(|| DEFAULT_REGISTRY_URL.clone());
        let registry = registry::RegistryClient::new(registry_url);
        Brioche {
            projects: Arc::new(RwLock::new(projects::Projects::default())),
            registry,
        }
    }
}
