use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use bstr::ByteVec as _;
use futures::{StreamExt as _, TryStreamExt as _};
use tokio::io::AsyncReadExt as _;

use crate::{
    process_events::{
        ProcessEvent, ProcessEventDescription, ProcessExitedEvent, ProcessSpawnedEvent,
    },
    recipe::{
        ArchiveFormat, Artifact, CompleteProcessRecipe, CompleteProcessTemplate,
        CompleteProcessTemplateComponent, CompressionFormat, DirectoryError, DownloadRecipe, Meta,
        ProcessRecipe, ProcessTemplate, ProcessTemplateComponent, Recipe, Unarchive, WithMeta,
    },
    reporter::{
        job::{NewJob, ProcessPacket, ProcessStatus, ProcessStream, UpdateJob},
        JobId,
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
    let mut env: BTreeMap<_, _> = futures::stream::iter(process.env)
        .then(|(key, artifact)| async move {
            let template =
                bake_lazy_process_template_to_process_template(brioche, scope, artifact).await?;
            anyhow::Ok((key, template))
        })
        .try_collect()
        .await?;

    let dependencies: Vec<_> = futures::stream::iter(process.dependencies)
        .then(|dependency| async move {
            let dependency = super::bake(brioche, dependency, scope).await?;
            anyhow::Ok(dependency)
        })
        .try_collect()
        .await?;
    append_dependency_envs(brioche, &mut env, dependencies.iter()).await?;

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
            ProcessTemplateComponent::ResourceDir => result
                .components
                .push(CompleteProcessTemplateComponent::ResourceDir),
            ProcessTemplateComponent::InputResourceDirs => result
                .components
                .push(CompleteProcessTemplateComponent::InputResourceDirs),
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

    // If the command is an absolute path, return it as-is
    if command_literal.starts_with(b"/") {
        return Ok(command);
    }

    // Otherwise, ensure the command does not look like a path
    anyhow::ensure!(
        !command_literal.contains(&b'/'),
        "command must not contain `/` unless it's an absolute path",
    );

    // Return an error if `$PATH` is not set by this point
    let Some(env_path) = env_path else {
        anyhow::bail!("tried to resolve {command_literal:?}, but process $PATH is not set");
    };

    // Split $PATH by `:`
    let path_templates = env_path.split_on_literal(":");
    let path_parts = path_templates.iter().map(|template| {
        match &*template.components {
            [CompleteProcessTemplateComponent::Input { artifact }, rest @ ..] => {
                // Ensure the rest of the path is a literal
                let subpath = CompleteProcessTemplate {
                    components: rest.to_vec(),
                };
                let Some(subpath) = subpath.as_literal() else {
                    anyhow::bail!("cannot resolve command {command:?}: $PATH component must be an artifact followed by a subpath");
                };

                // Get the subpath without the leading '/'
                let subpath = match subpath.split_first() {
                    None => b"",
                    Some((&b'/', subpath)) => subpath,
                    _ => {
                        anyhow::bail!("cannot resolve command {command:?}: invalid subpath {subpath:?}");
                    }
                };
                let subpath = bstr::BString::from(subpath);

                anyhow::Ok((artifact, subpath))
            }
            _ => {
                anyhow::bail!("cannot resolve command {command:?}: $PATH component must be an artifact followed by a subpath");
            }
        }
    }).collect::<anyhow::Result<Vec<_>>>()?;

    for (artifact, subpath) in path_parts {
        // Ensure the artifact is a directory
        let Artifact::Directory(dir) = &artifact.value else {
            continue;
        };

        // Get the artifact referred to by the subpath
        let subpath_artifact = dir.get(brioche, &subpath).await;
        let subpath_artifact = match &subpath_artifact {
            Ok(Some(subpath_artifact)) => subpath_artifact,
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
            Ok(Some(command_artifact)) => command_artifact,
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

        // Create a template for the command, with the artifact followed by
        // '/' followed by the subpath to the command
        let command_subpath = if subpath.is_empty() {
            bstr::join("", ["/".as_bytes(), &command_literal])
        } else {
            bstr::join(
                "",
                ["/".as_bytes(), &subpath, "/".as_bytes(), &command_literal],
            )
        };
        let command_template = CompleteProcessTemplate {
            components: vec![
                CompleteProcessTemplateComponent::Input {
                    artifact: artifact.clone(),
                },
                CompleteProcessTemplateComponent::Literal {
                    value: bstr::BString::new(command_subpath),
                },
            ],
        };

        return Ok(command_template);
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

    let created_at = std::time::Instant::now();
    let mut job_status = ProcessStatus::Preparing { created_at };
    let job_id = brioche.reporter.add_job(NewJob::Process {
        status: job_status.clone(),
    });

    let hash = Recipe::CompleteProcess(process.clone()).hash();

    let temp_dir = brioche.home.join("process-temp");
    let bake_dir = temp_dir.join(ulid::Ulid::new().to_string());
    let bake_dir = BakeDir::create(bake_dir).await?;
    let root_dir = bake_dir.path().join("root");
    tokio::fs::create_dir(&root_dir).await?;
    let output_dir = bake_dir.path().join("outputs");
    tokio::fs::create_dir(&output_dir).await?;
    let output_path = output_dir.join(format!("output-{hash}"));

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

    let guest_resource_dir = PathBuf::from("/brioche-resources.d");
    let relative_resource_dir = guest_resource_dir
        .strip_prefix("/")
        .expect("invalid guest resource dir");
    let host_resource_dir = root_dir.join(relative_resource_dir);
    let guest_resource_dir =
        Vec::<u8>::from_path_buf(guest_resource_dir).expect("failed to build resource dir path");
    tokio::fs::create_dir_all(&host_resource_dir).await?;

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
            &crate::recipe::Artifact::Directory(process.work_dir.clone()),
            crate::output::OutputOptions {
                output_path: &host_work_dir,
                merge: true,
                resource_dir: Some(&host_resource_dir),
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
                    resource_dir: Some(&host_resource_dir),
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

    let templates = [&process.command]
        .into_iter()
        .chain(&process.args)
        .chain(process.env.values());
    let mut host_input_resource_dirs = vec![];
    for template in templates {
        get_process_template_input_resource_dirs(brioche, template, &mut host_input_resource_dirs)
            .await?;
    }

    let mut host_guest_input_resource_dirs = vec![];
    for host_input_resource_dir in &host_input_resource_dirs {
        let resource_dir_name = host_input_resource_dir
            .file_name()
            .context("unexpected input resource dir path")?;
        let resource_dir_name = <[u8] as bstr::ByteSlice>::from_os_str(resource_dir_name)
            .context("invalid input resource dir name")?;
        let guest_input_resource_dir: bstr::BString = guest_home_dir
            .iter()
            .copied()
            .chain(b"/.local/share/brioche/locals/".iter().copied())
            .chain(resource_dir_name.iter().copied())
            .collect();

        host_guest_input_resource_dirs
            .push((host_input_resource_dir.to_owned(), guest_input_resource_dir));
    }

    let dirs = ProcessTemplateDirs {
        output_path: &output_path,
        host_resource_dir: &host_resource_dir,
        guest_resource_dir: &guest_resource_dir,
        host_guest_input_resource_dirs: &host_guest_input_resource_dirs,
        host_home_dir: &host_home_dir,
        guest_home_dir: &guest_home_dir,
        host_work_dir: &host_work_dir,
        guest_work_dir: &guest_work_dir,
        host_temp_dir: &host_temp_dir,
        guest_temp_dir: &guest_temp_dir,
    };

    let command = build_process_template(brioche, process.command.clone(), dirs).await?;
    let args = futures::stream::iter(process.args.clone())
        .then(|arg| build_process_template(brioche, arg, dirs))
        .try_collect::<Vec<_>>()
        .await?;

    let env = futures::stream::iter(process.env.clone())
        .then(|(key, artifact)| async move {
            let template = build_process_template(brioche, artifact, dirs).await?;
            anyhow::Ok((key, template))
        })
        .try_collect::<HashMap<_, _>>()
        .await?;

    let sandbox_config = SandboxExecutionConfig {
        sandbox_root: root_dir.clone(),
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

    let events_path = bake_dir.path().join("events.bin.zst");
    let event_writer = tokio::fs::File::create(&events_path).await?;
    let event_writer = tokio::io::BufWriter::new(event_writer);
    let event_writer = crate::utils::zstd::ZstdSeekableEncoder::new(event_writer, 3, 1024 * 1024)?;
    let mut event_writer =
        crate::process_events::writer::ProcessEventWriter::new(event_writer).await?;

    let events_started_at = std::time::Instant::now();
    let process_descirption = ProcessEventDescription {
        created_at: jiff::Zoned::now(),
        meta: Cow::Borrowed(meta),
        output_dir: Cow::Borrowed(&*output_dir),
        root_dir: Cow::Borrowed(&*root_dir),
        recipe: Cow::Borrowed(&process),
        sandbox_config: Cow::Borrowed(&sandbox_config),
    };
    event_writer
        .write_event(&ProcessEvent::Description(process_descirption))
        .await?;

    let result = if brioche.self_exec_processes {
        run_sandboxed_self_exec(
            brioche,
            sandbox_config,
            job_id,
            &mut job_status,
            events_started_at,
            &mut event_writer,
        )
        .await
    } else {
        run_sandboxed_inline(brioche, sandbox_config, job_id, &mut job_status).await
    };

    event_writer.shutdown().await?;

    match result {
        Ok(()) => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "process failed, view full output by runing `brioche jobs logs {}`",
                    events_path.display(),
                )
            });
        }
    }

    let result = crate::input::create_input(
        brioche,
        crate::input::InputOptions {
            input_path: &output_path,
            remove_input: true,
            resource_dir: Some(&host_resource_dir),
            input_resource_dirs: &host_input_resource_dirs,
            saved_paths: &mut HashMap::new(),
            meta,
        },
    )
    .await
    .context("failed to save outputs from process")?;

    if !brioche.keep_temps {
        bake_dir.remove().await?;
    }

    job_status.to_finalized(std::time::Instant::now())?;
    brioche.reporter.update_job(
        job_id,
        UpdateJob::ProcessUpdateStatus {
            status: job_status.clone(),
        },
    );

    Ok(result.value)
}

async fn run_sandboxed_inline(
    brioche: &Brioche,
    sandbox_config: SandboxExecutionConfig,
    job_id: JobId,
    job_status: &mut ProcessStatus,
) -> anyhow::Result<()> {
    job_status.to_running(std::time::Instant::now(), None)?;
    brioche.reporter.update_job(
        job_id,
        UpdateJob::ProcessUpdateStatus {
            status: job_status.clone(),
        },
    );

    let status =
        tokio::task::spawn_blocking(|| crate::sandbox::run_sandbox(sandbox_config)).await??;

    anyhow::ensure!(
        status.success(),
        "sandboxed process exited with non-zero status code"
    );

    job_status.to_ran(std::time::Instant::now())?;
    brioche.reporter.update_job(
        job_id,
        UpdateJob::ProcessUpdateStatus {
            status: job_status.clone(),
        },
    );

    Ok(())
}

async fn run_sandboxed_self_exec(
    brioche: &Brioche,
    sandbox_config: SandboxExecutionConfig,
    job_id: JobId,
    job_status: &mut ProcessStatus,
    events_started_at: std::time::Instant,
    event_writer: &mut crate::process_events::writer::ProcessEventWriter<
        impl tokio::io::AsyncWrite + Unpin,
    >,
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

    let child_id = child.id();
    let mut stdout = child.stdout.take().expect("failed to get stdout");
    let mut stderr = child.stderr.take().expect("failed to get stderr");

    job_status.to_running(std::time::Instant::now(), child_id)?;
    brioche.reporter.update_job(
        job_id,
        UpdateJob::ProcessUpdateStatus {
            status: job_status.clone(),
        },
    );

    event_writer
        .write_event(&ProcessEvent::Spawned(ProcessSpawnedEvent {
            elapsed: events_started_at.elapsed(),
            pid: child_id.unwrap_or(0),
        }))
        .await?;

    let mut stdout_buffer = vec![0; 1024 * 1024];
    let mut stderr_buffer = vec![0; 1024 * 1024];

    let wait_with_output_fut = child.wait_with_output();
    let mut wait_with_output_fut = std::pin::pin!(wait_with_output_fut);

    let output = loop {
        tokio::select! {
            bytes_read = stdout.read(&mut stdout_buffer) => {
                let buffer = &stdout_buffer[..bytes_read?];

                let events = crate::process_events::create_process_output_events(events_started_at.elapsed(), ProcessStream::Stdout, buffer);
                for event in events {
                    event_writer.write_event(&crate::process_events::ProcessEvent::Output(event)).await?;
                }

                brioche.reporter.update_job(
                    job_id,
                    UpdateJob::ProcessPushPacket {
                        packet: ProcessPacket::Stdout(buffer.into()).into()
                    },
                );
            }
            bytes_read = stderr.read(&mut stderr_buffer) => {
                let buffer = &stderr_buffer[..bytes_read?];

                let events = crate::process_events::create_process_output_events(events_started_at.elapsed(), ProcessStream::Stderr, buffer);
                for event in events {
                    event_writer.write_event(&crate::process_events::ProcessEvent::Output(event)).await?;
                }

                brioche.reporter.update_job(
                    job_id,
                    UpdateJob::ProcessPushPacket {
                        packet: ProcessPacket::Stderr(buffer.into()).into()
                    },
                );
            }
            output = wait_with_output_fut.as_mut() => {
                break output;
            },
        }
    };

    let output = output?;

    if !output.stdout.is_empty() {
        let events = crate::process_events::create_process_output_events(
            events_started_at.elapsed(),
            ProcessStream::Stderr,
            &output.stdout,
        );
        for event in events {
            event_writer
                .write_event(&ProcessEvent::Output(event))
                .await?;
        }

        brioche.reporter.update_job(
            job_id,
            UpdateJob::ProcessPushPacket {
                packet: ProcessPacket::Stdout(output.stdout).into(),
            },
        );
    }

    if !output.stderr.is_empty() {
        let events = crate::process_events::create_process_output_events(
            events_started_at.elapsed(),
            ProcessStream::Stderr,
            &output.stderr,
        );
        for event in events {
            event_writer
                .write_event(&ProcessEvent::Output(event))
                .await?;
        }

        brioche.reporter.update_job(
            job_id,
            UpdateJob::ProcessPushPacket {
                packet: ProcessPacket::Stdout(output.stderr).into(),
            },
        );
    }

    brioche
        .reporter
        .update_job(job_id, UpdateJob::ProcessFlushPackets);

    let exit_status: crate::sandbox::ExitStatus = output.status.into();
    event_writer
        .write_event(&ProcessEvent::Exited(ProcessExitedEvent {
            elapsed: events_started_at.elapsed(),
            exit_status,
        }))
        .await?;

    if !output.status.success() {
        anyhow::bail!("process exited with status code {}", output.status);
    }

    job_status.to_ran(std::time::Instant::now())?;
    brioche.reporter.update_job(
        job_id,
        UpdateJob::ProcessUpdateStatus {
            status: job_status.clone(),
        },
    );

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ProcessTemplateDirs<'a> {
    output_path: &'a Path,
    host_resource_dir: &'a Path,
    guest_resource_dir: &'a [u8],
    host_guest_input_resource_dirs: &'a [(PathBuf, bstr::BString)],
    host_home_dir: &'a Path,
    guest_home_dir: &'a [u8],
    host_work_dir: &'a Path,
    guest_work_dir: &'a [u8],
    host_temp_dir: &'a Path,
    guest_temp_dir: &'a [u8],
}

async fn get_process_template_input_resource_dirs(
    brioche: &Brioche,
    template: &CompleteProcessTemplate,
    resources: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    for component in &template.components {
        match component {
            CompleteProcessTemplateComponent::Input { artifact } => {
                let local_output =
                    crate::output::create_local_output(brioche, &artifact.value).await?;
                if let Some(resource_dir) = local_output.resource_dir {
                    resources.push(resource_dir);
                }
            }
            CompleteProcessTemplateComponent::Literal { .. }
            | CompleteProcessTemplateComponent::OutputPath
            | CompleteProcessTemplateComponent::ResourceDir
            | CompleteProcessTemplateComponent::InputResourceDirs
            | CompleteProcessTemplateComponent::HomeDir
            | CompleteProcessTemplateComponent::WorkDir
            | CompleteProcessTemplateComponent::TempDir => {}
        }
    }

    Ok(())
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
            CompleteProcessTemplateComponent::ResourceDir => {
                result
                    .components
                    .push(SandboxTemplateComponent::Path(SandboxPath {
                        host_path: dirs.host_resource_dir.to_owned(),
                        options: SandboxPathOptions {
                            mode: HostPathMode::ReadWriteCreate,
                            guest_path_hint: dirs.guest_resource_dir.into(),
                        },
                    }))
            }
            CompleteProcessTemplateComponent::InputResourceDirs => {
                for (n, (host, guest)) in dirs.host_guest_input_resource_dirs.iter().enumerate() {
                    if n > 0 {
                        result
                            .components
                            .push(SandboxTemplateComponent::Literal { value: b":".into() });
                    }

                    result
                        .components
                        .push(SandboxTemplateComponent::Path(SandboxPath {
                            host_path: host.to_owned(),
                            options: SandboxPathOptions {
                                mode: HostPathMode::Read,
                                guest_path_hint: guest.clone(),
                            },
                        }))
                }
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

enum DependencyEnvVarChange {
    AppendPath {
        artifact: WithMeta<Artifact>,
        subpath: bstr::BString,
    },
    FallbackPath {
        artifact: WithMeta<Artifact>,
        subpath: bstr::BString,
    },
    FallbackValue {
        value: bstr::BString,
    },
}

impl DependencyEnvVarChange {
    fn build_components(&self) -> Vec<CompleteProcessTemplateComponent> {
        match self {
            DependencyEnvVarChange::AppendPath { artifact, subpath }
            | DependencyEnvVarChange::FallbackPath { artifact, subpath } => {
                let mut subpath = subpath.clone();

                // Build template components representing either `${artifact}` or
                // `${artifact}/${subpath}`
                if subpath.is_empty() {
                    vec![CompleteProcessTemplateComponent::Input {
                        artifact: artifact.clone(),
                    }]
                } else {
                    subpath.insert(0, b'/');
                    vec![
                        CompleteProcessTemplateComponent::Input {
                            artifact: artifact.clone(),
                        },
                        CompleteProcessTemplateComponent::Literal { value: subpath },
                    ]
                }
            }
            DependencyEnvVarChange::FallbackValue { value } => {
                vec![CompleteProcessTemplateComponent::Literal {
                    value: value.clone(),
                }]
            }
        }
    }
}

async fn append_dependency_envs(
    brioche: &Brioche,
    env: &mut BTreeMap<bstr::BString, CompleteProcessTemplate>,
    dependencies: impl Iterator<Item = &WithMeta<Artifact>>,
) -> anyhow::Result<()> {
    // Tuples of env var values to add, where each artifact subpath will
    // be added to the env var separated by `:`
    // (env_var_name, artifact, artifact_subpath)
    let mut env_var_changes: Vec<(bstr::BString, DependencyEnvVarChange)> = vec![];

    for dependency_artifact in dependencies {
        // Validate that the dependency is a directory
        let Artifact::Directory(dependency) = &dependency_artifact.value else {
            anyhow::bail!("dependency must be a directory recipe");
        };

        // Get the directory `brioche-env.d/env` if it exists
        let env_dir = dependency.get(brioche, b"brioche-env.d/env").await?;
        let env_dir = env_dir.and_then(|env_dir| match env_dir {
            Artifact::Directory(dir) => Some(dir),
            _ => None,
        });
        if let Some(env_dir) = env_dir {
            // Each entry in the directory will get treated as an env var,
            // depending if it's a file, directory, or symlink
            let env_dir_entries = env_dir.entries(brioche).await?;

            for (env_var, env_dir_entry) in env_dir_entries {
                match env_dir_entry {
                    Artifact::Directory(env_dir_entry) => {
                        let env_value_entries = env_dir_entry.entries(brioche).await?;

                        // Each entry within the env var directory should be a symlink
                        // pointing to a path to append to the env var
                        for (env_value_entry_name, env_value_entry) in env_value_entries {
                            // Validate it's a symlink
                            let Artifact::Symlink {
                                target: env_value_target,
                            } = env_value_entry
                            else {
                                anyhow::bail!("expected `brioche-env.d/env/{env_var}/{env_value_entry_name}` to be a symlink");
                            };

                            // Get the path of the symlink relative to the
                            // root of the dependency artifact
                            let dependency_subpath = bstr::join(
                                "/",
                                [
                                    "brioche-env.d".as_bytes(),
                                    "env".as_bytes(),
                                    &**env_var,
                                    &*env_value_target,
                                ]
                                .into_iter(),
                            );
                            let dependency_subpath =
                                crate::fs_utils::logical_path_bytes(&dependency_subpath)?;

                            // Append the env var
                            env_var_changes.push((
                                env_var.clone(),
                                DependencyEnvVarChange::AppendPath {
                                    artifact: dependency_artifact.clone(),
                                    subpath: dependency_subpath.into(),
                                },
                            ));
                        }
                    }
                    Artifact::File(env_dir_file) => {
                        // Read the file to get the env var value
                        let mut permit = crate::blob::get_save_blob_permit().await?;
                        let env_dir_blob_path =
                            crate::blob::blob_path(brioche, &mut permit, env_dir_file.content_blob)
                                .await?;
                        let env_value = tokio::fs::read(env_dir_blob_path).await?;

                        // Add the env var
                        env_var_changes.push((
                            env_var.clone(),
                            DependencyEnvVarChange::FallbackValue {
                                value: env_value.into(),
                            },
                        ));
                    }
                    Artifact::Symlink {
                        target: env_value_target,
                    } => {
                        // Get the path of the symlink relative to the
                        // root of the dependency artifact
                        let dependency_subpath = bstr::join(
                            "/",
                            [
                                "brioche-env.d".as_bytes(),
                                "env".as_bytes(),
                                &*env_value_target,
                            ]
                            .into_iter(),
                        );
                        let dependency_subpath =
                            crate::fs_utils::logical_path_bytes(&dependency_subpath)?;

                        // Add the env var
                        env_var_changes.push((
                            env_var.clone(),
                            DependencyEnvVarChange::FallbackPath {
                                artifact: dependency_artifact.clone(),
                                subpath: dependency_subpath.into(),
                            },
                        ));
                    }
                }
            }
        }

        // If the artifact contains a `bin` directory, append that to `$PATH`
        // automatically
        let bin_artifact = dependency.get(brioche, b"bin").await?;
        if matches!(bin_artifact, Some(Artifact::Directory { .. })) {
            env_var_changes.push((
                "PATH".into(),
                DependencyEnvVarChange::AppendPath {
                    artifact: dependency_artifact.clone(),
                    subpath: "bin".into(),
                },
            ));
        }
    }

    // Append to the env vars
    for (env_var, change) in env_var_changes {
        // Get the current env var value
        let current_value = env
            .entry(env_var.clone())
            .or_insert_with(|| CompleteProcessTemplate { components: vec![] });

        let components = change.build_components();

        match change {
            DependencyEnvVarChange::AppendPath { .. } => {
                // If the current value is empty or unset, set it to the new value.
                // Otherwise, add a `:` and append the new value
                if current_value.is_empty() {
                    *current_value = CompleteProcessTemplate { components };
                } else {
                    current_value.append_literal(":");
                    current_value.components.extend(components);
                };
            }
            DependencyEnvVarChange::FallbackPath { .. }
            | DependencyEnvVarChange::FallbackValue { .. } => {
                // Only set the fallback value if the current value is unset
                if current_value.is_empty() {
                    *current_value = CompleteProcessTemplate { components };
                }
            }
        }
    }

    Ok(())
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
        resource_dir: None,
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
