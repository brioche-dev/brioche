use std::{collections::HashMap, path::PathBuf};

use crate::encoding::{AsPath, TickEncoded};

pub mod linux_namespace;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackend {
    LinuxNamespace(linux_namespace::LinuxNamespaceSandbox),
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxExecutionConfig {
    #[serde_as(as = "AsPath<TickEncoded>")]
    pub sandbox_root: PathBuf,
    #[serde_as(as = "HashMap<AsPath<TickEncoded>, _>")]
    pub include_host_paths: HashMap<PathBuf, SandboxPathOptions>,
    pub command: SandboxTemplate,
    pub args: Vec<SandboxTemplate>,
    #[serde_as(as = "HashMap<TickEncoded, _>")]
    pub env: HashMap<bstr::BString, SandboxTemplate>,
    pub current_dir: SandboxPath,
    pub networking: bool,
    pub uid_hint: u32,
    pub gid_hint: u32,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxPath {
    #[serde_as(as = "AsPath<TickEncoded>")]
    pub host_path: PathBuf,
    #[serde(flatten)]
    pub options: SandboxPathOptions,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxPathOptions {
    pub mode: HostPathMode,
    #[serde_as(as = "TickEncoded")]
    pub guest_path_hint: bstr::BString,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTemplate {
    pub components: Vec<SandboxTemplateComponent>,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
pub enum SandboxTemplateComponent {
    Literal {
        #[serde_as(as = "TickEncoded")]
        value: bstr::BString,
    },
    Path(SandboxPath),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPathMode {
    Read,
    ReadWriteCreate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitStatus {
    Code(i32),
    Signal(i32),
    Other { message: String },
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        matches!(self, Self::Code(0))
    }

    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Code(code) => Some(*code),
            _ => None,
        }
    }
}

impl From<std::process::ExitStatus> for ExitStatus {
    fn from(status: std::process::ExitStatus) -> Self {
        use std::os::unix::process::ExitStatusExt as _;

        #[allow(clippy::manual_map)]
        if let Some(signal) = status.signal() {
            Self::Signal(signal)
        } else if let Some(code) = status.code() {
            Self::Code(code)
        } else {
            Self::Other {
                message: status.to_string(),
            }
        }
    }
}

pub fn run_sandbox(
    backend: SandboxBackend,
    exec: SandboxExecutionConfig,
) -> anyhow::Result<ExitStatus> {
    match backend {
        SandboxBackend::LinuxNamespace(sandbox) => {
            cfg_if::cfg_if! {
                if #[cfg(target_os = "linux")] {
                    linux_namespace::run_sandbox(sandbox, exec)
                } else {
                    anyhow::bail!("tried to use Linux namespace sandbox backend, but it's not supported on this platform");
                }
            }
        }
    }
}
