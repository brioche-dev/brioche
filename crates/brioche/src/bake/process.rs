use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use bstr::ByteVec as _;
use futures::{StreamExt as _, TryStreamExt as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tracing::Instrument as _;

use crate::{
    recipe::{
        ArchiveFormat, Artifact, CompleteProcessRecipe, CompleteProcessTemplate,
        CompleteProcessTemplateComponent, CompressionFormat, DirectoryError, DownloadRecipe, Meta,
        ProcessRecipe, ProcessTemplate, ProcessTemplateComponent, Recipe, Unarchive, WithMeta,
    },
    sandbox::{
        HostPathMode, SandboxExecutionConfig, SandboxPath, SandboxPathOptions, SandboxTemplate,
        SandboxTemplateComponent,
    },
    Brioche,
};

const GUEST_UID_HINT: u32 = 1099;
const GUEST_GID_HINT: u32 = 1099;

#[tracing::instrument(skip(brioche, process))]
pub async fn bake_lazy_process_to_process(
    brioche: &Brioche,
    scope: &super::BakeScope,
    process: ProcessRecipe,
) -> anyhow::Result<CompleteProcessRecipe> {
    let unsafe_required = process.networking;

    if unsafe_required {
        anyhow::ensure!(
            process.is_unsafe,
            "to enable networking, `unsafe` must be set to true"
        );
    } else {
        anyhow::ensure!(
            !process.is_unsafe,
            "process is marked as unsafe but does not use any unsafe features"
        );
    }

    let command =
        bake_lazy_process_template_to_process_template(brioche, scope, process.command).await?;
    let args = futures::stream::iter(process.args)
        .then(|arg| bake_lazy_process_template_to_process_template(brioche, scope, arg))
        .try_collect()
        .await?;
    let env: BTreeMap<_, _> = futures::stream::iter(process.env)
        .then(|(key, artifact)| async move {
            let template =
                bake_lazy_process_template_to_process_template(brioche, scope, artifact).await?;
            anyhow::Ok((key, template))
        })
        .try_collect()
        .await?;

    let env_path = env.get(bstr::BStr::new("PATH"));
    let command = resolve_command(brioche, command, env_path).await?;

    let work_dir = super::bake(brioche, *process.work_dir, scope).await?;
    let crate::recipe::Artifact::Directory(work_dir) = work_dir.value else {
        anyhow::bail!("expected process workdir to be a directory artifact");
    };

    let output_scaffold = match process.output_scaffold {
        Some(output_scaffold) => {
            let output_scaffold = super::bake(brioche, *output_scaffold, scope).await?;
            Some(Box::new(output_scaffold.value))
        }
        None => None,
    };

    Ok(CompleteProcessRecipe {
        command,
        args,
        env,
        work_dir,
        output_scaffold,
        platform: process.platform,
        is_unsafe: process.is_unsafe,
        networking: process.networking,
    })
}

#[tracing::instrument(skip_all)]
async fn bake_lazy_process_template_to_process_template(
    brioche: &Brioche,
    scope: &super::BakeScope,
    template: ProcessTemplate,
) -> anyhow::Result<CompleteProcessTemplate> {
    let mut result = CompleteProcessTemplate { components: vec![] };
    for component in &template.components {
        match component {
            ProcessTemplateComponent::Literal { value } => {
                result
                    .components
                    .push(CompleteProcessTemplateComponent::Literal {
                        value: value.clone(),
                    })
            }
            ProcessTemplateComponent::Input { recipe } => {
                let artifact = super::bake(brioche, recipe.clone(), scope).await?;

                result
                    .components
                    .push(CompleteProcessTemplateComponent::Input { artifact });
            }
            ProcessTemplateComponent::OutputPath => result
                .components
                .push(CompleteProcessTemplateComponent::OutputPath),
            ProcessTemplateComponent::ResourcesDir => result
                .components
                .push(CompleteProcessTemplateComponent::ResourcesDir),
            ProcessTemplateComponent::HomeDir => result
                .components
                .push(CompleteProcessTemplateComponent::HomeDir),
            ProcessTemplateComponent::WorkDir => result
                .components
                .push(CompleteProcessTemplateComponent::WorkDir),
            ProcessTemplateComponent::TempDir => result
                .components
                .push(CompleteProcessTemplateComponent::TempDir),
        }
    }

    Ok(result)
}

/// Try to resolve a command template using a template `$PATH` env var. Returns
/// either a template that expands to the absolute path of the command, or
/// the original template if the command could not be explicitly resolved.
async fn resolve_command(
    brioche: &Brioche,
    command: CompleteProcessTemplate,
    env_path: Option<&CompleteProcessTemplate>,
) -> anyhow::Result<CompleteProcessTemplate> {
    // Return the original template if the command is not a literal. In this
    // case, it will probably expand to an absolute path within an artifact
    let Some(command_literal) = command.as_literal() else {
        return Ok(command);
    };

    // If the command template contains a `/`, assume it might be an
    // absolute path and use it as-is. This is similar to what Bash does
    if command_literal.contains(&b'/') {
        return Ok(command);
    }

    // Return an error if `$PATH` is not set by this point
    let Some(env_path) = env_path else {
        anyhow::bail!("tried to resolve {command_literal:?}, but process $PATH is not set");
    };

    // Split $PATH by `:`
    let path_templates = env_path.split_on_literal(":");
    for path_template in path_templates {
        // Match a template like std.tpl`${artifact}/foo` (subpath optional)
        if let [CompleteProcessTemplateComponent::Input { artifact }, rest @ ..] =
            &*path_template.components
        {
            // Ensure the rest of the path is a literal
            let subpath = CompleteProcessTemplate {
                components: rest.to_vec(),
            };
            let Some(subpath) = subpath.as_literal() else {
                continue;
            };

            // Ensure the artifact is a directory
            let Artifact::Directory(dir) = &artifact.value else {
                continue;
            };

            // Get the artifact referred to by the subpath
            let subpath_artifact = dir.get(brioche, &subpath).await;
            let subpath_artifact = match &subpath_artifact {
                Ok(Some(subpath_artifact)) => &subpath_artifact.value,
                Err(DirectoryError::EmptyPath { .. }) => {
                    // If the subpath was empty, use the directory itself
                    &artifact.value
                }
                _ => {
                    continue;
                }
            };

            // Ensure the subpath artifact is a directory
            let Artifact::Directory(subpath_dir) = subpath_artifact else {
                continue;
            };

            // Try to get the artifact referred to by the command
            let command_artifact = subpath_dir.get(brioche, &command_literal).await;
            let command_artifact = match &command_artifact {
                Ok(Some(command_artifact)) => &command_artifact.value,
                _ => {
                    continue;
                }
            };

            // Ensure the command artifact is either an executable file
            // or symlink
            match command_artifact {
                Artifact::File(crate::recipe::File {
                    executable: true, ..
                })
                | Artifact::Symlink { .. } => {}
                _ => {
                    continue;
                }
            }

            // Create a template with the command name added to the end
            let mut command_template = path_template.clone();
            command_template.append_literal("/");
            command_template.append_literal(&*command_literal);

            return Ok(command_template);
        }
    }

    // We didn't find the command, so return an error
    anyhow::bail!("{command_literal:?} not found in process $PATH");
}

#[tracing::instrument(skip(brioche, process))]
pub async fn bake_process(
    brioche: &Brioche,
    meta: &Arc<Meta>,
    process: CompleteProcessRecipe,
) -> anyhow::Result<Artifact> {
    tracing::debug!("acquiring process semaphore permit");
    let _permit = brioche.process_semaphore.acquire().await;
    tracing::debug!("acquired process semaphore permit");

    let hash = Recipe::CompleteProcess(process.clone()).hash();

    let temp_dir = brioche.home.join("process-temp");
    let bake_dir = temp_dir.join(ulid::Ulid::new().to_string());
    let bake_dir = BakeDir::create(bake_dir).await?;
    let root_dir = bake_dir.path().join("root");
    tokio::fs::create_dir(&root_dir).await?;
    let output_dir = bake_dir.path().join("outputs");
    tokio::fs::create_dir(&output_dir).await?;
    let output_path = output_dir.join(format!("output-{hash}"));
    let stdout_file = tokio::fs::File::create(bake_dir.path().join("stdout.log")).await?;
    let stderr_file = tokio::fs::File::create(bake_dir.path().join("stderr.log")).await?;
    let status_path = bake_dir.path().join("status.txt");

    // Generate a username and home directory in the sandbox based on
    // the process's hash. This is done so processes can't make assumptions
    // about what folder they run in, while also ensuring the home directory
    // path is fully deterministic.
    let guest_username = format!("brioche-runner-{hash}");
    let guest_home_dir = format!("/home/{guest_username}");
    set_up_rootfs(brioche, &root_dir, &guest_username, &guest_home_dir).await?;

    let guest_home_dir = PathBuf::from(guest_home_dir);
    let relative_home_dir = guest_home_dir
        .strip_prefix("/")
        .expect("invalid guest home dir");
    let host_home_dir = root_dir.join(relative_home_dir);
    let guest_home_dir =
        Vec::<u8>::from_path_buf(guest_home_dir.clone()).expect("failed to build home dir path");
    tokio::fs::create_dir_all(&host_home_dir).await?;

    let relative_work_dir = relative_home_dir.join("work");
    let host_work_dir = root_dir.join(&relative_work_dir);
    let guest_work_dir = PathBuf::from("/").join(&relative_work_dir);
    let guest_work_dir =
        Vec::<u8>::from_path_buf(guest_work_dir).expect("failed to build work dir path");
    tokio::fs::create_dir_all(&host_work_dir).await?;

    let guest_temp_dir = PathBuf::from("/tmp");
    let relative_emp_dir = guest_temp_dir
        .strip_prefix("/")
        .expect("invalid guest tmp dir");
    let host_temp_dir = root_dir.join(relative_emp_dir);
    let guest_temp_dir =
        Vec::<u8>::from_path_buf(guest_temp_dir).expect("failed to build tmp dir path");
    tokio::fs::create_dir_all(&host_temp_dir).await?;

    let guest_pack_dir = PathBuf::from("/brioche-pack.d");
    let relative_pack_dir = guest_pack_dir
        .strip_prefix("/")
        .expect("invalid guest pack dir");
    let host_pack_dir = root_dir.join(relative_pack_dir);
    let guest_pack_dir =
        Vec::<u8>::from_path_buf(guest_pack_dir).expect("failed to build pack dir path");
    tokio::fs::create_dir_all(&host_pack_dir).await?;

    if process.networking {
        let guest_etc_dir = root_dir.join("etc");
        tokio::fs::create_dir_all(&guest_etc_dir).await?;

        let resolv_conf_contents = tokio::fs::read("/etc/resolv.conf").await;
        let resolv_conf_contents = match resolv_conf_contents {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                vec![]
            }
            Err(error) => {
                return Err(error).context("failed to read host /etc/resolv.conf");
            }
        };
        tokio::fs::write(guest_etc_dir.join("resolv.conf"), &resolv_conf_contents)
            .await
            .context("failed to write guest /etc/resolv.conf")?;
    }

    let create_work_dir_fut = async {
        crate::output::create_output(
            brioche,
            &crate::recipe::Artifact::Directory(process.work_dir),
            crate::output::OutputOptions {
                output_path: &host_work_dir,
                merge: true,
                resources_dir: Some(&host_pack_dir),
                mtime: Some(crate::fs_utils::brioche_epoch()),
                link_locals: false,
            },
        )
        .await
    };
    let create_output_scaffold_fut = async {
        if let Some(output_scaffold) = &process.output_scaffold {
            crate::output::create_output(
                brioche,
                output_scaffold,
                crate::output::OutputOptions {
                    output_path: &output_path,
                    merge: false,
                    resources_dir: Some(&host_pack_dir),
                    mtime: Some(crate::fs_utils::brioche_epoch()),
                    link_locals: false,
                },
            )
            .await
        } else {
            Ok(())
        }
    };
    tokio::try_join!(create_work_dir_fut, create_output_scaffold_fut)?;

    let dirs = ProcessTemplateDirs {
        output_path: &output_path,
        host_resources_dir: &host_pack_dir,
        guest_resources_dir: &guest_pack_dir,
        host_home_dir: &host_home_dir,
        guest_home_dir: &guest_home_dir,
        host_work_dir: &host_work_dir,
        guest_work_dir: &guest_work_dir,
        host_temp_dir: &host_temp_dir,
        guest_temp_dir: &guest_temp_dir,
    };

    let command = build_process_template(brioche, process.command, dirs).await?;
    let args = futures::stream::iter(process.args)
        .then(|arg| build_process_template(brioche, arg, dirs))
        .try_collect::<Vec<_>>()
        .await?;

    let env = futures::stream::iter(process.env)
        .then(|(key, artifact)| async move {
            let template = build_process_template(brioche, artifact, dirs).await?;
            anyhow::Ok((key, template))
        })
        .try_collect::<HashMap<_, _>>()
        .await?;

    let sandbox_config = SandboxExecutionConfig {
        sandbox_root: root_dir,
        include_host_paths: HashMap::from_iter([
            (
                PathBuf::from("/dev"),
                SandboxPathOptions {
                    mode: HostPathMode::ReadWriteCreate,
                    guest_path_hint: "/dev".into(),
                },
            ),
            (
                PathBuf::from("/proc"),
                SandboxPathOptions {
                    mode: HostPathMode::ReadWriteCreate,
                    guest_path_hint: "/proc".into(),
                },
            ),
            (
                PathBuf::from("/sys"),
                SandboxPathOptions {
                    mode: HostPathMode::ReadWriteCreate,
                    guest_path_hint: "/sys".into(),
                },
            ),
            (
                host_temp_dir,
                SandboxPathOptions {
                    mode: HostPathMode::ReadWriteCreate,
                    guest_path_hint: guest_temp_dir.into(),
                },
            ),
        ]),
        command,
        args,
        env,
        current_dir: SandboxPath {
            host_path: host_work_dir,
            options: SandboxPathOptions {
                mode: HostPathMode::ReadWriteCreate,
                guest_path_hint: guest_work_dir.into(),
            },
        },
        networking: process.networking,
        uid_hint: GUEST_UID_HINT,
        gid_hint: GUEST_GID_HINT,
    };

    let result = if brioche.self_exec_processes {
        run_sandboxed_self_exec(brioche, sandbox_config, stdout_file, stderr_file).await
    } else {
        run_sandboxed_inline(sandbox_config).await
    };

    match result {
        Ok(()) => {}
        Err(error) => {
            tokio::fs::write(&status_path, error.to_string())
                .await
                .context("failed to write process status")?;
            return Err(error);
        }
    }

    let result = crate::input::create_input(
        brioche,
        crate::input::InputOptions {
            input_path: &output_path,
            remove_input: true,
            resources_dir: Some(&host_pack_dir),
            meta,
        },
    )
    .await
    .context("failed to save outputs from process")?;

    if !brioche.keep_temps {
        bake_dir.remove().await?;
    }

    Ok(result.value)
}

async fn run_sandboxed_inline(sandbox_config: SandboxExecutionConfig) -> anyhow::Result<()> {
    let status =
        tokio::task::spawn_blocking(|| crate::sandbox::run_sandbox(sandbox_config)).await??;

    anyhow::ensure!(
        status.success(),
        "sandboxed process exited with non-zero status code"
    );

    Ok(())
}

async fn run_sandboxed_self_exec(
    brioche: &Brioche,
    sandbox_config: SandboxExecutionConfig,
    write_stdout: impl tokio::io::AsyncWrite + Send + Sync + 'static,
    write_stderr: impl tokio::io::AsyncWrite + Send + Sync + 'static,
) -> anyhow::Result<()> {
    tracing::debug!(?sandbox_config, "running sandboxed process");

    let sandbox_config = serde_json::to_string(&sandbox_config)?;
    let brioche_exe = std::env::current_exe()?;
    let mut child = tokio::process::Command::new(brioche_exe)
        .args(["run-sandbox", "--config", &sandbox_config])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let start = std::time::Instant::now();
    let child_id = child.id();
    let mut stdout = child.stdout.take().expect("failed to get stdout");
    let mut stderr = child.stderr.take().expect("failed to get stderr");

    let mut job_status = crate::reporter::ProcessStatus::Running { child_id, start };
    let job_id = brioche.reporter.add_job(crate::reporter::NewJob::Process {
        status: job_status.clone(),
    });

    tokio::task::spawn({
        let brioche = brioche.clone();
        async move {
            let mut stdout_buffer = [0; 4096];
            let mut stderr_buffer = [0; 4096];
            let mut write_stdout = std::pin::pin!(write_stdout);
            let mut write_stderr = std::pin::pin!(write_stderr);
            loop {
                let packet = tokio::select! {
                    bytes_read = stdout.read(&mut stdout_buffer) => {
                        let buffer = &stdout_buffer[..bytes_read?];
                        write_stdout.write_all(buffer).await?;
                        crate::reporter::ProcessPacket::Stdout(buffer.to_vec())
                    }
                    bytes_read = stderr.read(&mut stderr_buffer) => {
                        let buffer = &stderr_buffer[..bytes_read?];
                        write_stderr.write_all(buffer).await?;
                        crate::reporter::ProcessPacket::Stdout(buffer.to_vec())
                    }
                };

                if packet.bytes().is_empty() {
                    break;
                }

                brioche.reporter.update_job(
                    job_id,
                    crate::reporter::UpdateJob::Process {
                        packet: Some(packet).into(),
                        status: job_status.clone(),
                    },
                )
            }

            anyhow::Ok(())
        }
    });

    let output = child.wait_with_output().await;
    let status = output.as_ref().ok().map(|output| output.status);

    job_status = crate::reporter::ProcessStatus::Exited {
        child_id,
        status,
        elapsed: start.elapsed(),
    };
    brioche.reporter.update_job(
        job_id,
        crate::reporter::UpdateJob::Process {
            packet: None.into(),
            status: job_status,
        },
    );

    let result = output?;
    if !result.status.success() {
        anyhow::bail!("process exited with status code {}", result.status);
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ProcessTemplateDirs<'a> {
    output_path: &'a Path,
    host_resources_dir: &'a Path,
    guest_resources_dir: &'a [u8],
    host_home_dir: &'a Path,
    guest_home_dir: &'a [u8],
    host_work_dir: &'a Path,
    guest_work_dir: &'a [u8],
    host_temp_dir: &'a Path,
    guest_temp_dir: &'a [u8],
}

async fn build_process_template(
    brioche: &Brioche,
    template: CompleteProcessTemplate,
    dirs: ProcessTemplateDirs<'_>,
) -> anyhow::Result<SandboxTemplate> {
    let output_parent = dirs.output_path.parent().context("invalid output path")?;
    let output_name = dirs
        .output_path
        .file_name()
        .context("invalid output path")?;
    let mut output_name = Vec::<u8>::from_os_string(output_name.to_owned())
        .map_err(|_| anyhow::anyhow!("invalid output path"))?;

    let mut output_parent_join = bstr::BString::new(vec![b'/']);
    output_parent_join.append(&mut output_name);

    let mut result = SandboxTemplate::default();
    for component in &template.components {
        match component {
            CompleteProcessTemplateComponent::Literal { value } => {
                result.components.push(SandboxTemplateComponent::Literal {
                    value: value.clone(),
                })
            }
            CompleteProcessTemplateComponent::Input { artifact } => {
                let local_output =
                    crate::output::create_local_output(brioche, &artifact.value).await?;

                if let Some(input_resources_dir) = &local_output.resources_dir {
                    tokio::task::spawn_blocking({
                        let input_resources_dir = input_resources_dir.clone();
                        let resources_dir = dirs.host_resources_dir.to_owned();
                        move || {
                            let input_resources_dir = std::fs::read_dir(&input_resources_dir)?;
                            let input_resources_dir_entries = input_resources_dir
                                .map(|entry| {
                                    let entry = entry?;
                                    let path = entry.path();
                                    std::io::Result::Ok(path.to_owned())
                                })
                                .collect::<std::io::Result<Vec<_>>>()?;
                            fs_extra::copy_items(
                                &input_resources_dir_entries,
                                &resources_dir,
                                &fs_extra::dir::CopyOptions::new().skip_exist(true),
                            )
                        }
                    })
                    .instrument(tracing::info_span!(
                        "copy_input_resouces_dir",
                        ?input_resources_dir
                    ))
                    .await?
                    .map_err(|e| anyhow::anyhow!("failed to copy resources dir: {}", e))?;
                }

                // $HOME/.local/share/brioche/locals/$HASH
                let guest_local_path: bstr::BString = dirs
                    .guest_home_dir
                    .iter()
                    .copied()
                    .chain(b"/.local/share/brioche/locals/".iter().copied())
                    .chain(artifact.value.hash().to_string().bytes())
                    .collect();
                result
                    .components
                    .push(SandboxTemplateComponent::Path(SandboxPath {
                        host_path: local_output.path,
                        options: SandboxPathOptions {
                            mode: HostPathMode::Read,
                            guest_path_hint: guest_local_path,
                        },
                    }));
            }
            CompleteProcessTemplateComponent::OutputPath => {
                // $HOME/.local/share/brioche/outputs
                let guest_outputs_path: bstr::BString = dirs
                    .guest_home_dir
                    .iter()
                    .chain(b"/.local/share/brioche/outputs".iter())
                    .copied()
                    .collect();
                result.components.extend([
                    SandboxTemplateComponent::Path(SandboxPath {
                        host_path: output_parent.to_owned(),
                        options: SandboxPathOptions {
                            mode: HostPathMode::ReadWriteCreate,
                            guest_path_hint: guest_outputs_path,
                        },
                    }),
                    SandboxTemplateComponent::Literal {
                        value: output_parent_join.clone(),
                    },
                ]);
            }
            CompleteProcessTemplateComponent::ResourcesDir => {
                result
                    .components
                    .push(SandboxTemplateComponent::Path(SandboxPath {
                        host_path: dirs.host_resources_dir.to_owned(),
                        options: SandboxPathOptions {
                            mode: HostPathMode::ReadWriteCreate,
                            guest_path_hint: dirs.guest_resources_dir.into(),
                        },
                    }))
            }
            CompleteProcessTemplateComponent::HomeDir => {
                result
                    .components
                    .push(SandboxTemplateComponent::Path(SandboxPath {
                        host_path: dirs.host_home_dir.to_owned(),
                        options: SandboxPathOptions {
                            mode: HostPathMode::ReadWriteCreate,
                            guest_path_hint: dirs.guest_home_dir.into(),
                        },
                    }))
            }
            CompleteProcessTemplateComponent::WorkDir => {
                result
                    .components
                    .push(SandboxTemplateComponent::Path(SandboxPath {
                        host_path: dirs.host_work_dir.to_owned(),
                        options: SandboxPathOptions {
                            mode: HostPathMode::ReadWriteCreate,
                            guest_path_hint: dirs.guest_work_dir.into(),
                        },
                    }))
            }
            CompleteProcessTemplateComponent::TempDir => {
                result
                    .components
                    .push(SandboxTemplateComponent::Path(SandboxPath {
                        host_path: dirs.host_temp_dir.to_owned(),
                        options: SandboxPathOptions {
                            mode: HostPathMode::ReadWriteCreate,
                            guest_path_hint: dirs.guest_temp_dir.into(),
                        },
                    }))
            }
        }
    }

    Ok(result)
}

#[tracing::instrument(skip(brioche))]
async fn set_up_rootfs(
    brioche: &Brioche,
    rootfs_dir: &Path,
    guest_username: &str,
    guest_home_dir: &str,
) -> anyhow::Result<()> {
    let output_rootfs_options = crate::output::OutputOptions {
        output_path: rootfs_dir,
        merge: true,
        resources_dir: None,
        mtime: None,
        link_locals: true,
    };

    let dash = Recipe::Unarchive(Unarchive {
        archive: ArchiveFormat::Tar,
        compression: CompressionFormat::Zstd,
        file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
            url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/dash_amd64_linux.tar.zstd".parse()?,
            hash: crate::Hash::Sha256 { value: hex::decode("ff52ae7e883ee4cbb0878f0e17decc18cd80b364147881fb576440e72e0129b2")? }
        }))),
    });
    let env = Recipe::Unarchive(Unarchive {
        archive: ArchiveFormat::Tar,
        compression: CompressionFormat::Zstd,
        file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
            url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/env_amd64_linux.tar.zstd".parse()?,
            hash: crate::Hash::Sha256 { value: hex::decode("8f5b15a9b5c695663ca2caefa0077c3889fcf65793c9a20ceca4ab12c7007453")? }
        }))),
    });

    tracing::debug!("resolving rootfs dash/env dependencies");
    let dash_and_env = super::bake(
        brioche,
        WithMeta::without_meta(Recipe::Merge {
            directories: vec![WithMeta::without_meta(dash), WithMeta::without_meta(env)],
        }),
        &super::BakeScope::Anonymous,
    )
    .await?;
    crate::output::create_output(brioche, &dash_and_env.value, output_rootfs_options).await?;

    tracing::trace!("building rootfs");

    let tmp_dir = rootfs_dir.join("tmp");
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .context("failed to create tmp")?;

    let usr_bin_dir = rootfs_dir.join("usr").join("bin");
    tokio::fs::create_dir_all(&usr_bin_dir)
        .await
        .context("failed to create usr")?;

    tokio::fs::symlink("/bin/env", usr_bin_dir.join("env"))
        .await
        .context("failed to symlink env")?;

    let etc_dir = rootfs_dir.join("etc");
    tokio::fs::create_dir_all(&etc_dir)
        .await
        .context("failed to create etc")?;

    let etc_passwd_contents = format!(
        "{guest_username}:!x:{GUEST_UID_HINT}:{GUEST_GID_HINT}::{guest_home_dir}:/bin/sh\n",
    );
    tokio::fs::write(etc_dir.join("passwd"), &etc_passwd_contents).await?;

    tracing::trace!("built rootfs");

    Ok(())
}

struct BakeDir {
    path: Option<PathBuf>,
}

impl BakeDir {
    async fn create(path: PathBuf) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(&path).await?;
        Ok(Self { path: Some(path) })
    }

    fn path(&self) -> &Path {
        self.path.as_ref().expect("bake dir not found")
    }

    async fn remove(mut self) -> anyhow::Result<()> {
        let path = self.path.take().context("bake dir not found")?;

        // Ensure that directories are writable so we can recursively remove
        // all files
        crate::fs_utils::set_directory_rwx_recursive(&path)
            .await
            .context("failed to set permissions for temprorary bake directory")?;

        tokio::fs::remove_dir_all(&path)
            .await
            .context("failed to remove temporary bake directory")?;
        Ok(())
    }
}

impl Drop for BakeDir {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            tracing::info!("keeping temporary bake dir {}", path.display());
        }
    }
}
