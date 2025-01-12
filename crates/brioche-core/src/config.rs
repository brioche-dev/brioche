use std::path::PathBuf;

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct BriocheConfig {
    pub registry_url: Option<url::Url>,

    #[serde(default)]
    pub sandbox: SandboxConfig,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "backend")]
#[serde(rename_all = "snake_case")]
pub enum SandboxConfig {
    #[default]
    Auto,
    LinuxNamespace(SandboxLinuxNamespaceConfig),
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct SandboxLinuxNamespaceConfig {
    pub proot: PRootConfig,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum PRootConfig {
    Value(bool),
    Custom { path: PathBuf },
}

impl Default for PRootConfig {
    fn default() -> Self {
        PRootConfig::Value(false)
    }
}
