use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use bstr::ByteVec as _;
use futures::{StreamExt as _, TryStreamExt as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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
        HostPathMode, SandboxBackend, SandboxExecutionConfig, SandboxPath, SandboxPathOptions,
        SandboxTemplate, SandboxTemplateComponent,
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
    let current_platform = crate::platform::current_platform();
    anyhow::ensure!(
        process.platform == current_platform,
        "tried to bake process for platform {}, but only {current_platform} is supported",
        process.platform,
    );
    let backend = sandbox_backend(brioche, process.platform).await?;

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
    set_up_rootfs(
        brioche,
        process.platform,
        &root_dir,
        &guest_username,
        &guest_home_dir,
    )
    .await?;

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
    let relative_temp_dir = guest_temp_dir
        .strip_prefix("/")
        .expect("invalid guest tmp dir");
    let host_temp_dir = root_dir.join(relative_temp_dir);
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
    let (mut event_writer_tx, mut event_writer_rx) = tokio::sync::mpsc::channel(100);

    // Spawn a task to write events so we can cleanly shut down the event writer
    let event_writer_task = brioche.task_tracker.spawn({
        let events_path = events_path.clone();
        let cancellation_token = brioche.cancellation_token.clone();

        async move {
            let event_writer = tokio::fs::File::create(&events_path).await?;
            let event_writer = tokio::io::BufWriter::new(event_writer);
            let event_writer = zstd_framed::AsyncZstdWriter::builder(event_writer)
                .with_seek_table(1024 * 1024)
                .build()?;
            let mut event_writer =
                crate::process_events::writer::ProcessEventWriter::new(event_writer).await?;

            loop {
                tokio::select! {
                    action = event_writer_rx.recv() => {
                        match action {
                            Some(ProcessEventWriterAction::ProcessEvent(event)) => {
                                event_writer.write_event(&event).await?;
                            }
                            Some(ProcessEventWriterAction::FinishFrameAndFlush) => {
                                let writer = event_writer.inner_mut();

                                writer.finish_frame()?;
                                writer.flush().await?;
                            }
                            None => {
                                break;
                            }
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                        break;
                    }
                }
            }

            event_writer.shutdown().await?;

            anyhow::Ok(())
        }
    });

    let events_started_at = std::time::Instant::now();
    let process_description = ProcessEventDescription {
        created_at: jiff::Zoned::now(),
        meta: (**meta).clone(),
        output_dir,
        root_dir,
        recipe: process,
        sandbox_config: sandbox_config.clone(),
    };
    event_writer_tx
        .send(ProcessEvent::Description(process_description).into())
        .await?;
    event_writer_tx
        .send(ProcessEventWriterAction::FinishFrameAndFlush)
        .await?;

    let result = if brioche.self_exec_processes {
        run_sandboxed_self_exec(
            brioche,
            backend,
            sandbox_config,
            job_id,
            &mut job_status,
            events_started_at,
            &mut event_writer_tx,
        )
        .await
    } else {
        run_sandboxed_inline(brioche, backend, sandbox_config, job_id, &mut job_status).await
    };

    drop(event_writer_tx);
    event_writer_task.await??;

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

enum ProcessEventWriterAction {
    ProcessEvent(ProcessEvent),
    FinishFrameAndFlush,
}

impl From<ProcessEvent> for ProcessEventWriterAction {
    fn from(event: ProcessEvent) -> Self {
        Self::ProcessEvent(event)
    }
}

async fn run_sandboxed_inline(
    brioche: &Brioche,
    backend: SandboxBackend,
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
        tokio::task::spawn_blocking(|| crate::sandbox::run_sandbox(backend, sandbox_config))
            .await??;

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
    backend: SandboxBackend,
    sandbox_config: SandboxExecutionConfig,
    job_id: JobId,
    job_status: &mut ProcessStatus,
    events_started_at: std::time::Instant,
    event_writer_tx: &mut tokio::sync::mpsc::Sender<ProcessEventWriterAction>,
) -> anyhow::Result<()> {
    tracing::debug!(?sandbox_config, "running sandboxed process");

    let sandbox_config = serde_json::to_string(&sandbox_config)?;
    let brioche_exe = std::env::current_exe()?;
    let backend = serde_json::to_string(&backend)?;
    let mut child = tokio::process::Command::new(brioche_exe)
        .args([
            "run-sandbox",
            "--backend",
            &backend,
            "--config",
            &sandbox_config,
        ])
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

    event_writer_tx
        .send(
            ProcessEvent::Spawned(ProcessSpawnedEvent {
                elapsed: events_started_at.elapsed(),
                pid: child_id.unwrap_or(0),
            })
            .into(),
        )
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
                    event_writer_tx.send(ProcessEvent::Output(event).into()).await?;
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
                    event_writer_tx.send(ProcessEvent::Output(event).into()).await?;
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
            event_writer_tx
                .send(ProcessEvent::Output(event).into())
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
            event_writer_tx
                .send(ProcessEvent::Output(event).into())
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
    event_writer_tx
        .send(
            ProcessEvent::Exited(ProcessExitedEvent {
                elapsed: events_started_at.elapsed(),
                exit_status,
            })
            .into(),
        )
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

async fn sandbox_backend(
    brioche: &Brioche,
    platform: crate::platform::Platform,
) -> anyhow::Result<SandboxBackend> {
    let backend = brioche
        .sandbox_backend
        .get_or_try_init(|| select_sandbox_backend(brioche, platform))
        .await?;
    Ok(backend.clone())
}

#[tracing::instrument(skip(brioche))]
async fn select_sandbox_backend(
    brioche: &Brioche,
    platform: crate::platform::Platform,
) -> anyhow::Result<SandboxBackend> {
    let rootfs_recipes = process_rootfs_recipes(platform);

    tracing::debug!("resolving rootfs sh/env dependencies");
    let rootfs_artifacts = super::bake(
        brioche,
        WithMeta::without_meta(Recipe::Merge {
            directories: vec![
                WithMeta::without_meta(rootfs_recipes.sh),
                WithMeta::without_meta(rootfs_recipes.env),
                WithMeta::without_meta(rootfs_recipes.utils),
            ],
        }),
        &super::BakeScope::Anonymous,
    )
    .await?;
    let rootfs_recipes_output =
        crate::output::create_local_output(brioche, &rootfs_artifacts.value).await?;

    let start = std::time::Instant::now();
    tracing::trace!("finding sandbox backend");

    let temp_dir = brioche
        .home
        .join("process-temp")
        .join(format!("{}-setup", ulid::Ulid::new()));
    let bake_dir = BakeDir::create(temp_dir).await?;
    let bake_dir = bake_dir.remove_on_drop();

    let root_dir = bake_dir.path().join("root");
    tokio::fs::create_dir(&root_dir).await?;
    let outputs_dir = bake_dir.path().join("outputs");
    tokio::fs::create_dir(&outputs_dir).await?;
    let work_dir = bake_dir.path().join("work");
    tokio::fs::create_dir(&work_dir).await?;

    let output_path = outputs_dir.join("output");
    let rootfs_recipes_sandbox_path = SandboxPath {
        host_path: rootfs_recipes_output.path,
        options: SandboxPathOptions {
            mode: HostPathMode::Read,
            guest_path_hint: bstr::BString::from("/rootfs-recipes"),
        },
    };
    let mut sandbox_config_env = HashMap::from_iter([
        (
            "PATH".into(),
            SandboxTemplate {
                components: vec![
                    SandboxTemplateComponent::Path(rootfs_recipes_sandbox_path.clone()),
                    SandboxTemplateComponent::Literal {
                        value: "/bin".into(),
                    },
                ],
            },
        ),
        (
            "BRIOCHE_OUTPUT".into(),
            SandboxTemplate {
                components: vec![
                    SandboxTemplateComponent::Path(SandboxPath {
                        host_path: outputs_dir,
                        options: SandboxPathOptions {
                            guest_path_hint: "/outputs".into(),
                            mode: HostPathMode::ReadWriteCreate,
                        },
                    }),
                    SandboxTemplateComponent::Literal {
                        value: "/output".into(),
                    },
                ],
            },
        ),
    ]);
    if let Some(resource_dir) = rootfs_recipes_output.resource_dir {
        sandbox_config_env.insert(
            "BRIOCHE_INPUT_RESOURCE_DIRS".into(),
            SandboxTemplate {
                components: vec![SandboxTemplateComponent::Path(SandboxPath {
                    host_path: resource_dir,
                    options: SandboxPathOptions {
                        guest_path_hint: "/rootfs-recipes-resources.d".into(),
                        mode: HostPathMode::ReadWriteCreate,
                    },
                })],
            },
        );
    }

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
        ]),
        command: SandboxTemplate {
            components: vec![
                SandboxTemplateComponent::Path(rootfs_recipes_sandbox_path.clone()),
                SandboxTemplateComponent::Literal {
                    value: "/bin/sh".into(),
                },
            ],
        },
        args: vec![
            SandboxTemplate {
                components: vec![SandboxTemplateComponent::Literal { value: "-c".into() }],
            },
            SandboxTemplate {
                components: vec![SandboxTemplateComponent::Literal {
                    value: r##"
                        set -eu
                        mkdir "$BRIOCHE_OUTPUT"/new
                        touch "$BRIOCHE_OUTPUT"/new/touch.txt
                        echo -n 'alpha' > "$BRIOCHE_OUTPUT"/existing/write.txt
                        echo -n 'bravo' >> "$BRIOCHE_OUTPUT"/existing/append.txt
                        echo -n > "$BRIOCHE_OUTPUT"/existing/truncate.txt
                        rm "$BRIOCHE_OUTPUT"/existing/remove.txt
                        rm "$BRIOCHE_OUTPUT"/remove_dir/remove.txt
                        rmdir "$BRIOCHE_OUTPUT"/remove_dir
                    "##
                    .into(),
                }],
            },
        ],
        env: sandbox_config_env,
        current_dir: SandboxPath {
            host_path: work_dir,
            options: SandboxPathOptions {
                guest_path_hint: "/work".into(),
                mode: HostPathMode::ReadWriteCreate,
            },
        },
        networking: false,
        uid_hint: GUEST_UID_HINT,
        gid_hint: GUEST_GID_HINT,
    };

    let backend =
        SandboxBackend::LinuxNamespace(crate::sandbox::linux_namespace::LinuxNamespaceSandbox {
            mount_style: crate::sandbox::linux_namespace::MountStyle::Namespace,
        });
    if check_sandbox_backend(&backend, &sandbox_config, &output_path).await? {
        bake_dir.remove().await?;

        tracing::trace!(
            "found sandbox backend in {}s",
            start.elapsed().as_secs_f32()
        );
        return Ok(backend);
    }

    if let Some(proot_recipe) = rootfs_recipes.proot {
        let proot_artifact = super::bake(
            brioche,
            WithMeta::without_meta(proot_recipe),
            &super::BakeScope::Anonymous,
        )
        .await?;
        let proot_output =
            crate::output::create_local_output(brioche, &proot_artifact.value).await?;
        assert!(
            proot_output.resource_dir.is_none(),
            "PRoot recipe includes a resource directory"
        );

        let backend = SandboxBackend::LinuxNamespace(
            crate::sandbox::linux_namespace::LinuxNamespaceSandbox {
                mount_style: crate::sandbox::linux_namespace::MountStyle::PRoot {
                    proot_path: proot_output.path.join("bin/proot"),
                },
            },
        );
        if check_sandbox_backend(&backend, &sandbox_config, &output_path).await? {
            bake_dir.remove().await?;

            tracing::warn!(
                "failed to run process sandbox with mount namespace, falling back to PRoot"
            );
            tracing::trace!(
                "found sandbox backend in {}s",
                start.elapsed().as_secs_f32()
            );
            return Ok(backend);
        }
    }

    bake_dir.remove().await?;
    anyhow::bail!("could not find a working backend to run processes");
}

async fn check_sandbox_backend(
    backend: &SandboxBackend,
    config: &SandboxExecutionConfig,
    output_path: &Path,
) -> anyhow::Result<bool> {
    let result = tokio::task::spawn_blocking({
        let backend = backend.clone();
        let config = config.clone();
        let output_path = output_path.to_owned();
        move || check_sandbox_backend_sync(backend, config, &output_path)
    })
    .await??;
    Ok(result)
}

fn check_sandbox_backend_sync(
    backend: SandboxBackend,
    config: SandboxExecutionConfig,
    output_path: &Path,
) -> anyhow::Result<bool> {
    // Destroy the output path in case it was created in a previous run
    let _ = std::fs::remove_file(output_path);
    let _ = std::fs::remove_dir_all(output_path);

    // Create the initial state for the output path
    std::fs::create_dir(output_path)?;
    std::fs::create_dir(output_path.join("existing"))?;
    std::fs::create_dir(output_path.join("remove_dir"))?;
    std::fs::write(output_path.join("existing").join("append.txt"), "alpha")?;
    std::fs::write(output_path.join("existing").join("truncate.txt"), "alpha")?;
    std::fs::write(output_path.join("existing").join("remove.txt"), "alpha")?;
    std::fs::write(output_path.join("remove_dir").join("remove.txt"), "alpha")?;

    let status = crate::sandbox::run_sandbox(backend, config);

    match status {
        Ok(status) if status.success() => {
            sanity_check_sandbox_output(output_path)
                .context("sandbox returned success, but sanity check failed")?;
            Ok(true)
        }
        Ok(status) => {
            anyhow::bail!("backend failed with status: {status:?}");
        }
        Err(error) => {
            tracing::trace!("backend did not succeed: {error:#?}");
            Ok(false)
        }
    }
}

fn sanity_check_sandbox_output(output_path: &Path) -> anyhow::Result<()> {
    let create_path = output_path.join("new/touch.txt");
    let write_path = output_path.join("existing/write.txt");
    let append_path = output_path.join("existing/append.txt");
    let truncate_path = output_path.join("existing/truncate.txt");
    let remove_path = output_path.join("existing/remove.txt");
    let remove_dir_path = output_path.join("remove_dir");

    std::fs::read(create_path)
        .context("failed to read new/touch.txt")
        .and_then(|content| {
            anyhow::ensure!(content.is_empty(), "new/touch.txt does not match");
            Ok(())
        })?;
    std::fs::read(write_path)
        .context("failed to read existing/write.txt")
        .and_then(|content| {
            anyhow::ensure!(content == b"alpha", "existing/write.txt does not match");
            Ok(())
        })?;
    std::fs::read_to_string(append_path)
        .context("failed to read existing/append.txt")
        .and_then(|content| {
            anyhow::ensure!(
                content == "alphabravo",
                "existing/append.txt does not match"
            );
            Ok(())
        })?;
    std::fs::read(truncate_path)
        .context("failed to read existing/truncate.txt")
        .and_then(|content| {
            anyhow::ensure!(content.is_empty(), "existing/truncate.txt does not match");
            Ok(())
        })?;
    match std::fs::metadata(remove_path) {
        Ok(_) => anyhow::bail!("existing/remove.txt exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("failed to read existing/remove.txt");
        }
    };
    match std::fs::metadata(remove_dir_path) {
        Ok(_) => anyhow::bail!("remove_dir exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("failed to read remove_dir");
        }
    };

    Ok(())
}

#[tracing::instrument(skip(brioche))]
async fn set_up_rootfs(
    brioche: &Brioche,
    platform: crate::platform::Platform,
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

    let recipes = process_rootfs_recipes(platform);

    tracing::debug!("resolving rootfs sh/env dependencies");
    let sh_and_env = super::bake(
        brioche,
        WithMeta::without_meta(Recipe::Merge {
            directories: vec![
                WithMeta::without_meta(recipes.sh),
                WithMeta::without_meta(recipes.env),
            ],
        }),
        &super::BakeScope::Anonymous,
    )
    .await?;
    crate::output::create_output(brioche, &sh_and_env.value, output_rootfs_options).await?;

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
    remove_on_drop: bool,
}

impl BakeDir {
    async fn create(path: PathBuf) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(&path).await?;
        Ok(Self {
            path: Some(path),
            remove_on_drop: false,
        })
    }

    fn remove_on_drop(mut self) -> Self {
        self.remove_on_drop = true;
        self
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
            .context("failed to set permissions for temporary bake directory")?;

        tokio::fs::remove_dir_all(&path)
            .await
            .context("failed to remove temporary bake directory")?;
        Ok(())
    }
}

impl Drop for BakeDir {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            if self.remove_on_drop {
                let _ = crate::fs_utils::set_directory_rwx_recursive_sync(path);
                let _ = std::fs::remove_dir_all(path);
            } else {
                tracing::info!("keeping temporary bake dir {}", path.display());
            }
        }
    }
}

pub struct ProcessRootfsRecipes {
    pub sh: Recipe,
    pub env: Recipe,
    pub utils: Recipe,
    pub proot: Option<Recipe>,
}

pub fn process_rootfs_recipes(platform: crate::platform::Platform) -> ProcessRootfsRecipes {
    match platform {
        crate::platform::Platform::X86_64Linux => {
            let sh = Recipe::Unarchive(Unarchive {
                archive: ArchiveFormat::Tar,
                compression: CompressionFormat::Zstd,
                file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
                    url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/dash_amd64_linux.tar.zstd".parse().unwrap(),
                    hash: crate::Hash::Sha256 { value: hex::decode("ff52ae7e883ee4cbb0878f0e17decc18cd80b364147881fb576440e72e0129b2").unwrap() }
                }))),
            });
            let env = Recipe::Unarchive(Unarchive {
                archive: ArchiveFormat::Tar,
                compression: CompressionFormat::Zstd,
                file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
                    url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/env_amd64_linux.tar.zstd".parse().unwrap(),
                    hash: crate::Hash::Sha256 { value: hex::decode("8f5b15a9b5c695663ca2caefa0077c3889fcf65793c9a20ceca4ab12c7007453").unwrap() }
                }))),
            });
            let utils = Recipe::Unarchive(Unarchive {
                archive: ArchiveFormat::Tar,
                compression: CompressionFormat::Zstd,
                file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
                    url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/utils_amd64_linux.tar.zstd".parse().unwrap(),
                    hash: crate::Hash::Sha256 { value: hex::decode("eb29ea059fcd9ca457841f5c79151721a74761a31610d694bce61a62f4de6d33").unwrap() }
                }))),
            });
            let proot = Recipe::Peel {
                directory: Box::new(WithMeta::without_meta(Recipe::Unarchive(Unarchive {
                    archive: ArchiveFormat::Tar,
                    compression: CompressionFormat::Zstd,
                    file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
                        url: "https://development-content.brioche.dev/github.com/brioche-dev/brioche-packages/812e80250e15793a33c19b770320bcc455f30b13/x86_64-linux/proot.tar.zstd".parse().unwrap(),
                        hash: crate::Hash::Sha256 { value: hex::decode("f0f4fb4dbe0dd4b050d4336c11d77c1c6b233ee944a4922f3f3a6eaabaedf385").unwrap() }
                    }))),
                }))),
                depth: 1,
            };

            ProcessRootfsRecipes {
                sh,
                env,
                utils,
                proot: Some(proot),
            }
        }
        crate::platform::Platform::Aarch64Linux => {
            let sh = Recipe::Unarchive(Unarchive {
                archive: ArchiveFormat::Tar,
                compression: CompressionFormat::Zstd,
                file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
                    url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/dash_arm64_linux.tar.zstd".parse().unwrap(),
                    hash: crate::Hash::Sha256 { value: hex::decode("29ac173dee09ff377fd49d3451a382d79273c79780b8841c9860dfc2d4b353d2").unwrap() }
                }))),
            });
            let env = Recipe::Unarchive(Unarchive {
                archive: ArchiveFormat::Tar,
                compression: CompressionFormat::Zstd,
                file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
                    url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/env_arm64_linux.tar.zstd".parse().unwrap(),
                    hash: crate::Hash::Sha256 { value: hex::decode("0b84d04b2768b6803aee78da8d54393ccbfe8730950b3cc305e8347fff5ad3d7").unwrap() }
                }))),
            });
            let utils = Recipe::Unarchive(Unarchive {
                archive: ArchiveFormat::Tar,
                compression: CompressionFormat::Zstd,
                file: Box::new(WithMeta::without_meta(Recipe::Download(DownloadRecipe {
                    url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/utils_arm64_linux.tar.zstd".parse().unwrap(),
                    hash: crate::Hash::Sha256 { value: hex::decode("36c0dcfb02e61a07f4654e1ca6047cdefb17ce1f2e37fcb2ba5dc20695b3e273").unwrap() }
                }))),
            });

            ProcessRootfsRecipes {
                sh,
                env,
                utils,
                // TODO: Build PRoot for aarch64
                proot: None,
            }
        }
    }
}
