use std::path::PathBuf;

use brioche_core::{
    process_events::{ProcessEvent, ProcessEventDescription, reader::ProcessEventReader},
    sandbox::{SandboxTemplate, SandboxTemplateComponent},
};
use clap::Parser;

use super::{log_file_reader_from_path, log_file_reader_from_stdin};

#[derive(Debug, Parser)]
pub struct DebugShellArgs {
    /// The path to the event file from the job
    path: PathBuf,
}

pub async fn debug_shell(args: &DebugShellArgs) -> anyhow::Result<()> {
    let job_description = tokio::task::spawn_blocking({
        let path = args.path.clone();
        move || {
            let input = if path.to_str() == Some("-") {
                log_file_reader_from_stdin()?
            } else {
                log_file_reader_from_path(&path)?
            };

            let mut reader = ProcessEventReader::new(input)?;
            let job_description = read_job_description_event(&mut reader)?;
            anyhow::Ok(job_description)
        }
    })
    .await??;

    let backend = {
        let (reporter, _guard) = brioche_core::reporter::console::start_console_reporter(
            brioche_core::reporter::console::ConsoleReporterKind::Plain,
        )?;

        let brioche = brioche_core::BriocheBuilder::new(reporter.clone())
            .build()
            .await?;
        crate::start_shutdown_handler(brioche.clone());

        brioche_core::bake::process::sandbox_backend(&brioche, job_description.recipe.platform)
            .await?
    };

    let sandbox_config = brioche_core::sandbox::SandboxExecutionConfig {
        command: SandboxTemplate {
            components: vec![SandboxTemplateComponent::Literal {
                value: b"/bin/sh".into(),
            }],
        },
        args: vec![SandboxTemplate {
            components: vec![SandboxTemplateComponent::Literal {
                value: b"-i".into(),
            }],
        }],
        ..job_description.sandbox_config.clone()
    };

    let exit_status = tokio::task::spawn_blocking(move || {
        brioche_core::sandbox::run_sandbox(backend, sandbox_config)
    })
    .await??;
    if !exit_status.success() {
        anyhow::bail!("sandbox execution failed: {exit_status:?}");
    }

    Ok(())
}

fn read_job_description_event(
    reader: &mut ProcessEventReader<impl std::io::Read>,
) -> anyhow::Result<ProcessEventDescription> {
    for _ in 0..2 {
        let event = reader.read_next_event()?;

        let Some(event) = event else {
            break;
        };

        if let ProcessEvent::Description(description) = event {
            return Ok(description);
        }
    }

    anyhow::bail!("failed to read `Description` event from event file");
}
