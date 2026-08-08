use std::{borrow::Cow, collections::HashMap, ffi::OsString};

use bstr::ByteSlice as _;

use crate::{SandboxError, SandboxResult};

use super::{SandboxPath, SandboxTemplate, SandboxTemplateComponent};

pub fn run_sandbox(exec: &super::SandboxExecutionConfig) -> SandboxResult<super::ExitStatus> {
    let program = build_template(&exec.command, &|| "command".into())?;
    let args = exec
        .args
        .iter()
        .enumerate()
        .map(|(n, arg)| build_template(arg, &|| format!("arg {n}").into()))
        .collect::<SandboxResult<Vec<_>>>()?;
    let env = exec
        .env
        .iter()
        .map(|(key, value)| {
            let env_key = key.to_os_str().map_err(|error| SandboxError::InvalidUtf8 {
                error,
                reason: "env var".into(),
            })?;
            let value = build_template(value, &|| format!("env var {key}").into())?;
            SandboxResult::Ok((env_key.to_os_string(), value))
        })
        .collect::<SandboxResult<HashMap<_, _>>>()?;
    let current_dir = build_template(&exec.current_dir, &|| "current dir".into())?;

    let program_path = std::path::Path::new(&program);
    if !program_path.is_absolute() {
        return Err(SandboxError::CommandIsNotAnAbsolutePath {
            program_path: program_path.to_path_buf(),
        });
    }

    let mut command = std::process::Command::new(program_path);
    command.args(args);
    command.env_clear();
    command.envs(env);
    command.current_dir(current_dir);

    let mut child = command.spawn().map_err(|error| SandboxError::IoError {
        error,
        reason: "failed to spawn sandbox".into(),
    })?;

    let exit_status = child.wait().map_err(|error| SandboxError::IoError {
        error,
        reason: "sandbox process failed".into(),
    })?;

    Ok(exit_status.into())
}

fn build_template(
    template: &SandboxTemplate,
    reason: &dyn Fn() -> Cow<'static, str>,
) -> SandboxResult<OsString> {
    let mut result = OsString::new();
    for component in &template.components {
        match component {
            SandboxTemplateComponent::Literal { value } => {
                let value = value
                    .to_os_str()
                    .map_err(|error| SandboxError::InvalidUtf8 {
                        error,
                        reason: reason(),
                    })?;
                result.push(value);
            }
            SandboxTemplateComponent::Path(SandboxPath { host_path, .. }) => {
                result.push(host_path);
            }
        }
    }

    Ok(result)
}
