use std::path::PathBuf;
use std::{collections::HashMap, ffi::OsString};

use anyhow::Context as _;
use bstr::ByteSlice as _;

use super::{SandboxPath, SandboxTemplate, SandboxTemplateComponent};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MountStyle {
    Namespace,
    PRoot { proot_path: PathBuf },
}

pub fn run_sandbox(exec: &super::SandboxExecutionConfig) -> anyhow::Result<super::ExitStatus> {
    let program = build_template(&exec.command)?;
    let args = exec
        .args
        .iter()
        .map(build_template)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let env = exec
        .env
        .iter()
        .map(|(key, value)| {
            let key = key.to_os_str()?.to_owned();
            let value = build_template(value)?;
            anyhow::Ok((key, value))
        })
        .collect::<anyhow::Result<HashMap<_, _>>>()?;

    let current_dir = build_template(&SandboxTemplate {
        components: vec![SandboxTemplateComponent::Path(exec.current_dir.clone())],
    })?;

    let program_path = std::path::Path::new(&program);
    anyhow::ensure!(
        program_path.is_absolute(),
        "expected command to resolve to an absolute path: {}",
        program.to_string_lossy()
    );

    let mut command = std::process::Command::new(program_path);
    command.args(args);
    command.env_clear();
    command.envs(env);
    command.current_dir(current_dir);

    let mut child = command
        .spawn()
        .map_err(|error| anyhow::anyhow!("failed to spawn unsandboxed process: {error}"))?;

    let exit_status = child.wait()?;

    Ok(exit_status.into())
}

fn build_template(template: &SandboxTemplate) -> anyhow::Result<OsString> {
    let mut result = OsString::new();
    for component in &template.components {
        match component {
            SandboxTemplateComponent::Literal { value } => {
                let value = value
                    .to_os_str()
                    .context("failed to convert string to OsString")?;
                result.push(value);
            }
            SandboxTemplateComponent::Path(SandboxPath { host_path, .. }) => {
                result.push(host_path);
            }
        }
    }

    Ok(result)
}
