use std::{collections::HashMap, ffi::OsString, path::PathBuf};

use bstr::ByteSlice as _;

use crate::encoding::{AsPath, UrlEncoded};

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxExecutionConfig {
    #[serde_as(as = "AsPath<UrlEncoded>")]
    pub sandbox_root: PathBuf,
    #[serde_as(as = "HashMap<AsPath<UrlEncoded>, _>")]
    pub include_host_paths: HashMap<PathBuf, SandboxPathOptions>,
    pub command: SandboxTemplate,
    pub args: Vec<SandboxTemplate>,
    #[serde_as(as = "HashMap<UrlEncoded, _>")]
    pub env: HashMap<bstr::BString, SandboxTemplate>,
    pub current_dir: SandboxPath,
    pub uid_hint: u32,
    pub gid_hint: u32,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxPath {
    #[serde_as(as = "AsPath<UrlEncoded>")]
    pub host_path: PathBuf,
    #[serde(flatten)]
    pub options: SandboxPathOptions,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxPathOptions {
    pub mode: HostPathMode,
    #[serde_as(as = "UrlEncoded")]
    pub guest_path_hint: bstr::BString,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTemplate {
    pub components: Vec<SandboxTemplateComponent>,
}

#[serde_with::serde_as]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
pub enum SandboxTemplateComponent {
    Literal {
        #[serde_as(as = "UrlEncoded")]
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

pub fn run_sandbox(exec: SandboxExecutionConfig) -> anyhow::Result<unshare::ExitStatus> {
    let mut host_paths = exec.include_host_paths;

    let sandbox_host_dir = exec.sandbox_root.join("mnt").join("brioche-host");
    std::fs::create_dir_all(&sandbox_host_dir)?;

    let program = build_template(&exec.command, &mut host_paths)?;
    let args = exec
        .args
        .iter()
        .map(|arg| build_template(arg, &mut host_paths))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let env = exec
        .env
        .iter()
        .map(|(key, value)| {
            let key = key.to_os_str()?.to_owned();
            let value = build_template(value, &mut host_paths)?;
            anyhow::Ok((key, value))
        })
        .collect::<anyhow::Result<HashMap<_, _>>>()?;

    let mut command = unshare::Command::new(program);
    command.args(&args);
    command.env_clear();
    command.envs(env);

    let current_dir = build_template(
        &SandboxTemplate {
            components: vec![SandboxTemplateComponent::Path(exec.current_dir.clone())],
        },
        &mut host_paths,
    )?;
    command.current_dir(current_dir);

    let host_uid = nix::unistd::Uid::current().as_raw();
    let host_gid = nix::unistd::Gid::current().as_raw();
    command.set_id_maps(
        vec![unshare::UidMap {
            inside_uid: exec.uid_hint,
            outside_uid: host_uid,
            count: 1,
        }],
        vec![unshare::GidMap {
            inside_gid: exec.gid_hint,
            outside_gid: host_gid,
            count: 1,
        }],
    );
    command.uid(exec.uid_hint);
    command.gid(exec.gid_hint);
    command.deny_setgroups(true);

    command.unshare([
        &unshare::Namespace::Mount,
        &unshare::Namespace::Net,
        &unshare::Namespace::User,
    ]);

    command.pivot_root(&exec.sandbox_root, &sandbox_host_dir, true);
    command.before_chroot({
        let sandbox_root = exec.sandbox_root.clone();
        move || {
            for (path, options) in &host_paths {
                let path_metadata = path.metadata().map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!(
                            "error getting metadata for path {}: {error}",
                            path.display()
                        ),
                    )
                })?;

                let guest_path = options.guest_path_hint.to_path().map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::Other, "invalid guest path")
                })?;
                let guest_path_under_root = guest_path.strip_prefix("/").map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("invalid guest path: {error}"),
                    )
                })?;
                let dest_path = sandbox_root.join(guest_path_under_root);

                // Create either an empty file or directory as the mountpoint
                if path_metadata.is_dir() {
                    std::fs::create_dir_all(&dest_path)?;
                } else {
                    if let Some(dest_parent) = dest_path.parent() {
                        std::fs::create_dir_all(dest_parent)?;
                    }

                    std::fs::write(&dest_path, "")?;
                }

                let readonly = match options.mode {
                    HostPathMode::Read => true,
                    HostPathMode::ReadWriteCreate => false,
                };

                libmount::BindMount::new(path, &dest_path)
                    .readonly(readonly)
                    .recursive(true)
                    .mount()
                    .map_err(|error| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!(
                                "failed to mount {} -> {}: {error}",
                                path.display(),
                                dest_path.display()
                            ),
                        )
                    })?;
            }

            libmount::BindMount::new(&sandbox_root, &sandbox_root)
                .recursive(true)
                .readonly(true)
                .mount()
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("failed to remount rootfs: {error}"),
                    )
                })?;

            Ok(())
        }
    });

    let mut child = command
        .spawn()
        .map_err(|error| anyhow::anyhow!("failed to spawn sandbox: {error}"))?;

    let result = child.wait()?;

    Ok(result)
}

fn build_template(
    template: &SandboxTemplate,
    host_paths: &mut HashMap<PathBuf, SandboxPathOptions>,
) -> anyhow::Result<OsString> {
    let mut result = bstr::BString::default();
    for component in &template.components {
        match component {
            SandboxTemplateComponent::Literal { value } => {
                result.extend_from_slice(value);
            }
            SandboxTemplateComponent::Path(SandboxPath { host_path, options }) => {
                let existing_options = host_paths.insert(host_path.clone(), options.clone());
                if let Some(existing_options) = existing_options {
                    anyhow::ensure!(
                        existing_options == *options,
                        "tried to mount host path {} with conflicting mount options",
                        host_path.display()
                    );
                }

                let guest_path = &options.guest_path_hint;
                result.extend_from_slice(guest_path);
            }
        }
    }

    let result = result.to_os_str()?;
    Ok(result.to_owned())
}
