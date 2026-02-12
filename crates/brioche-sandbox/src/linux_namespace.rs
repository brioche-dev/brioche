use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::{collections::HashMap, ffi::OsString};

#[cfg(target_os = "linux")]
use bstr::ByteSlice as _;

#[cfg(target_os = "linux")]
use super::{SandboxPath, SandboxPathOptions, SandboxTemplate, SandboxTemplateComponent};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MountStyle {
    Namespace,
    PRoot { proot_path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LinuxNamespaceSandbox {
    pub mount_style: MountStyle,
}

#[cfg(target_os = "linux")]
#[expect(clippy::similar_names)]
pub fn run_sandbox(
    sandbox: LinuxNamespaceSandbox,
    exec: super::SandboxExecutionConfig,
) -> anyhow::Result<super::ExitStatus> {
    let mut host_paths = exec.include_host_paths;

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

    let current_dir = build_template(&exec.current_dir, &mut host_paths)?;

    let mut unshare_namespaces = vec![unshare::Namespace::User];

    // Unshare the network namespace if networking is disabled
    if !exec.networking {
        unshare_namespaces.push(unshare::Namespace::Net);
    }

    let mut command: unshare::Command;
    match sandbox.mount_style {
        MountStyle::Namespace => {
            unshare_namespaces.push(unshare::Namespace::Mount);

            command = unshare::Command::new(program);
            command.args(&args);
            command.env_clear();
            command.envs(env);
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
            command.unshare(&unshare_namespaces);

            let sandbox_host_dir = exec.sandbox_root.join("mnt").join("brioche-host");
            std::fs::create_dir_all(&sandbox_host_dir)?;

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

                        let guest_path = options
                            .guest_path_hint
                            .to_path()
                            .map_err(|_| std::io::Error::other("invalid guest path"))?;
                        let guest_path_under_root =
                            guest_path.strip_prefix("/").map_err(|error| {
                                std::io::Error::other(format!("invalid guest path: {error}"))
                            })?;
                        let dest_path = sandbox_root.join(guest_path_under_root);

                        // Ensure the mount destination exists by creating
                        // either an empty file or directory if the path
                        // doesn't exist
                        if path_metadata.is_dir() {
                            std::fs::create_dir_all(&dest_path)?;
                        } else {
                            if let Some(dest_parent) = dest_path.parent() {
                                std::fs::create_dir_all(dest_parent)?;
                            }

                            let result = std::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(&dest_path);
                            match result {
                                Ok(_) => {
                                    // Empty file created
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                                    // Path already exists, so no need to
                                    // create it. This can happen when using
                                    // a path that was already created within
                                    // the sandbox root, such as CA certs
                                }
                                Err(error) => {
                                    return Err(error);
                                }
                            }
                        }

                        let readonly = match options.mode {
                            super::HostPathMode::Read => true,
                            super::HostPathMode::ReadWriteCreate => false,
                        };

                        libmount::BindMount::new(path, &dest_path)
                            .readonly(readonly)
                            .recursive(true)
                            .mount()
                            .map_err(|error| {
                                std::io::Error::other(format!(
                                    "failed to mount {} -> {}: {error}",
                                    path.display(),
                                    dest_path.display()
                                ))
                            })?;
                    }

                    libmount::BindMount::new(&sandbox_root, &sandbox_root)
                        .recursive(true)
                        .readonly(true)
                        .mount()
                        .map_err(|error| {
                            std::io::Error::other(format!("failed to remount rootfs: {error}"))
                        })?;

                    Ok(())
                }
            });
        }
        MountStyle::PRoot { proot_path } => {
            command = unshare::Command::new(proot_path);

            command.arg("--kill-on-exit");

            command.arg("-r");
            command.arg(exec.sandbox_root);

            command.arg("-w");
            command.arg(&current_dir);

            for (host_path, options) in host_paths {
                let mut arg = std::ffi::OsString::from("--bind=");
                arg.push(host_path);
                arg.push(":");
                arg.push(options.guest_path_hint.to_os_str()?);
                arg.push("!");

                command.arg(arg);
            }

            command.arg(&program);
            command.args(&args);
            command.env_clear();
            command.envs(env);

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
            command.unshare(&unshare_namespaces);
        }
    }

    let mut child = command
        .spawn()
        .map_err(|error| anyhow::anyhow!("failed to spawn sandbox: {error}"))?;

    let exit_status = child.wait()?;

    let exit_status = match exit_status {
        unshare::ExitStatus::Exited(code) => super::ExitStatus::Code(code.into()),
        unshare::ExitStatus::Signaled(signal, _) => super::ExitStatus::Signal(signal as i32),
    };

    Ok(exit_status)
}

#[cfg(target_os = "linux")]
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
