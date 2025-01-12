#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct BriocheConfig {
    pub registry_url: Option<url::Url>,
}
