use std::{borrow::Cow, time::Duration};

use jiff::Zoned;

use brioche_core::{
    process_events::{
        create_process_output_events, reader::ProcessEventReader, writer::ProcessEventWriter,
        CreateProcessOutputEventError, ProcessEvent, ProcessEventDescription, ProcessExitedEvent,
        ProcessOutputEvent, ProcessSpawnedEvent, PROCESS_EVENT_MAGIC,
    },
    recipe::{CompleteProcessRecipe, CompleteProcessTemplate, Meta},
    reporter::job::ProcessStream,
    sandbox::{ExitStatus, SandboxExecutionConfig, SandboxPath, SandboxPathOptions},
};

pub fn default_complete_process() -> CompleteProcessRecipe {
    CompleteProcessRecipe {
        command: CompleteProcessTemplate { components: vec![] },
        args: vec![],
        env: Default::default(),
        work_dir: Default::default(),
        output_scaffold: None,
        platform: brioche_core::platform::current_platform(),
        is_unsafe: false,
        networking: false,
    }
}

fn example_process_event_description() -> ProcessEventDescription<'static> {
    ProcessEventDescription {
        recipe: Cow::Owned(default_complete_process()),
        meta: Cow::Owned(Meta::default()),
        sandbox_config: Cow::Owned(SandboxExecutionConfig {
            sandbox_root: Default::default(),
            include_host_paths: Default::default(),
            command: Default::default(),
            args: Default::default(),
            env: Default::default(),
            current_dir: SandboxPath {
                host_path: Default::default(),
                options: SandboxPathOptions {
                    mode: brioche_core::sandbox::HostPathMode::Read,
                    guest_path_hint: Default::default(),
                },
            },
            gid_hint: 0,
            uid_hint: 0,
            networking: false,
        }),
        created_at: Zoned::now(),
        root_dir: Default::default(),
        output_dir: Default::default(),
    }
}

fn example_events() -> Vec<ProcessEvent<'static>> {
    vec![
        ProcessEvent::Description(example_process_event_description()),
        ProcessEvent::Spawned(ProcessSpawnedEvent {
            elapsed: Duration::from_secs(1),
            pid: 123,
        }),
        ProcessEvent::Output(
            ProcessOutputEvent::new(
                Duration::from_secs(2),
                ProcessStream::Stdout,
                Cow::Owned("foo".into()),
            )
            .unwrap(),
        ),
        ProcessEvent::Output(
            ProcessOutputEvent::new(
                Duration::from_secs(3),
                ProcessStream::Stderr,
                Cow::Owned("bar".into()),
            )
            .unwrap(),
        ),
        ProcessEvent::Exited(ProcessExitedEvent {
            elapsed: Duration::from_secs(4),
            exit_status: ExitStatus::Code(0),
        }),
    ]
}

fn example_events_with_signal() -> Vec<ProcessEvent<'static>> {
    vec![
        ProcessEvent::Description(example_process_event_description()),
        ProcessEvent::Spawned(ProcessSpawnedEvent {
            elapsed: Duration::from_secs(1),
            pid: 123,
        }),
        ProcessEvent::Output(
            ProcessOutputEvent::new(
                Duration::from_secs(2),
                ProcessStream::Stdout,
                Cow::Owned("foo".into()),
            )
            .unwrap(),
        ),
        ProcessEvent::Output(
            ProcessOutputEvent::new(
                Duration::from_secs(3),
                ProcessStream::Stderr,
                Cow::Owned("bar".into()),
            )
            .unwrap(),
        ),
        ProcessEvent::Exited(ProcessExitedEvent {
            elapsed: Duration::from_secs(4),
            exit_status: ExitStatus::Signal(9),
        }),
    ]
}

#[tokio::test]
async fn test_process_event_read_and_write_empty() -> anyhow::Result<()> {
    let mut buffer = vec![];

    // Should only write the magic string
    {
        let writer = std::io::Cursor::new(&mut buffer);
        let mut writer = ProcessEventWriter::new(writer).await?;

        writer.shutdown().await?;
    }

    // Should read the same initial event
    let mut read_events = vec![];
    {
        let reader = std::io::Cursor::new(&buffer);
        let mut reader = ProcessEventReader::new(reader).await?;

        while let Some(event) = reader.read_next_event().await? {
            read_events.push(event);
        }
    }

    assert_eq!(buffer, PROCESS_EVENT_MAGIC.as_bytes());

    assert!(read_events.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_process_event_read_and_write_minimal() -> anyhow::Result<()> {
    let mut buffer = vec![];
    let description = example_process_event_description();

    // Should write one initial event
    {
        let writer = std::io::Cursor::new(&mut buffer);
        let mut writer = ProcessEventWriter::new(writer).await?;

        writer
            .write_event(&ProcessEvent::Description(description.clone()))
            .await?;

        writer.shutdown().await?;
    }

    // Should read the same initial event
    let mut read_events = vec![];
    {
        let reader = std::io::Cursor::new(&buffer);
        let mut reader = ProcessEventReader::new(reader).await?;

        while let Some(event) = reader.read_next_event().await? {
            read_events.push(event);
        }
    }

    assert_eq!(read_events, [ProcessEvent::Description(description)]);

    Ok(())
}

#[tokio::test]
async fn test_process_event_read_and_write_sequence() -> anyhow::Result<()> {
    let mut buffer = vec![];
    let events = example_events();

    // Should write each event
    {
        let writer = std::io::Cursor::new(&mut buffer);
        let mut writer = ProcessEventWriter::new(writer).await?;

        for event in &events {
            writer.write_event(event).await?;
        }

        writer.shutdown().await?;
    }

    // Should read the same set of events
    let mut read_events = vec![];
    {
        let reader = std::io::Cursor::new(&buffer);
        let mut reader = ProcessEventReader::new(reader).await?;

        while let Some(event) = reader.read_next_event().await? {
            read_events.push(event);
        }
    }

    assert_eq!(read_events, events);

    Ok(())
}

#[tokio::test]
async fn test_process_event_read_and_write_sequence_with_signal() -> anyhow::Result<()> {
    let mut buffer = vec![];
    let events = example_events_with_signal();

    // Should write the description followed by each event
    {
        let writer = std::io::Cursor::new(&mut buffer);
        let mut writer = ProcessEventWriter::new(writer).await?;

        for event in &events {
            writer.write_event(event).await?;
        }

        writer.shutdown().await?;
    }

    // Should read the description event followed by the other events
    let mut read_events = vec![];
    {
        let reader = std::io::Cursor::new(&buffer);
        let mut reader = ProcessEventReader::new(reader).await?;

        while let Some(event) = reader.read_next_event().await? {
            read_events.push(event);
        }
    }

    assert_eq!(read_events, events);

    Ok(())
}

#[tokio::test]
async fn test_process_event_read_reverse() -> anyhow::Result<()> {
    let mut buffer = vec![];
    let events = example_events();

    // Write each event
    {
        let writer = std::io::Cursor::new(&mut buffer);
        let mut writer = ProcessEventWriter::new(writer).await?;

        for event in &events {
            writer.write_event(event).await?;
        }

        writer.shutdown().await?;
    }

    // Seek to the end, then read the events back to the beginning
    let mut read_events = vec![];
    {
        let reader = std::io::Cursor::new(&buffer);
        let mut reader = ProcessEventReader::new(reader).await?;

        reader.seek_to_end().await?;

        while let Some(event) = reader.read_previous_event().await? {
            read_events.push(event);
        }
    }

    // Reverse the list to get back the original order
    read_events.reverse();

    assert_eq!(read_events, events);

    Ok(())
}

#[tokio::test]
async fn test_process_event_read_first_then_reverse() -> anyhow::Result<()> {
    let mut buffer = vec![];
    let events = example_events();

    // Write each event
    {
        let writer = std::io::Cursor::new(&mut buffer);
        let mut writer = ProcessEventWriter::new(writer).await?;

        for event in &events {
            writer.write_event(event).await?;
        }

        writer.shutdown().await?;
    }

    // Read one event, then seek to the end, then read all events
    // in reverse order. This is a common pattern to get the description
    // of a process first, followed by reading events from the end
    let first_event;
    let mut read_events = vec![];
    {
        let reader = std::io::Cursor::new(&buffer);
        let mut reader = ProcessEventReader::new(reader).await?;

        first_event = reader.read_next_event().await?;

        reader.seek_to_end().await?;

        while let Some(event) = reader.read_previous_event().await? {
            read_events.push(event);
        }
    }

    // Reverse the list to get back the original order
    read_events.reverse();

    assert_eq!(first_event.as_ref(), Some(&events[0]));
    assert_eq!(read_events, events);

    Ok(())
}

#[test]
fn test_process_event_create_output_event() {
    let result =
        ProcessOutputEvent::new(Duration::ZERO, ProcessStream::Stdout, Cow::Owned("".into()));
    assert_matches::assert_matches!(result, Err(CreateProcessOutputEventError::EmptyContent));

    let result = ProcessOutputEvent::new(
        Duration::ZERO,
        ProcessStream::Stdout,
        Cow::Owned("aaaa".into()),
    );
    assert_eq!(result.unwrap().content(), "aaaa");

    let result = ProcessOutputEvent::new(
        Duration::ZERO,
        ProcessStream::Stdout,
        Cow::Owned(vec![0; ProcessOutputEvent::MAX_CONTENT_LENGTH].into()),
    );
    assert_eq!(
        result.unwrap().content().len(),
        ProcessOutputEvent::MAX_CONTENT_LENGTH
    );

    let result = ProcessOutputEvent::new(
        Duration::ZERO,
        ProcessStream::Stdout,
        Cow::Owned(vec![0; ProcessOutputEvent::MAX_CONTENT_LENGTH + 1].into()),
    );
    assert_matches::assert_matches!(
        result.err(),
        Some(CreateProcessOutputEventError::ContentTooLong { .. })
    );
}

#[test]
fn test_process_event_create_output_events() {
    let event_lengths =
        create_process_output_events(Duration::ZERO, ProcessStream::Stdout, &[0; 0])
            .map(|event| event.content().len())
            .collect::<Vec<_>>();
    assert!(event_lengths.is_empty());

    let event_lengths =
        create_process_output_events(Duration::ZERO, ProcessStream::Stdout, &[0; 5])
            .map(|event| event.content().len())
            .collect::<Vec<_>>();
    assert_eq!(event_lengths, [5]);

    let event_lengths = create_process_output_events(
        Duration::ZERO,
        ProcessStream::Stdout,
        &[0; ProcessOutputEvent::MAX_CONTENT_LENGTH],
    )
    .map(|event| event.content().len())
    .collect::<Vec<_>>();
    assert_eq!(event_lengths, [ProcessOutputEvent::MAX_CONTENT_LENGTH]);

    let event_lengths = create_process_output_events(
        Duration::ZERO,
        ProcessStream::Stdout,
        &[0; ProcessOutputEvent::MAX_CONTENT_LENGTH + 1],
    )
    .map(|event| event.content().len())
    .collect::<Vec<_>>();
    assert_eq!(event_lengths, [ProcessOutputEvent::MAX_CONTENT_LENGTH, 1]);

    let event_lengths = create_process_output_events(
        Duration::ZERO,
        ProcessStream::Stdout,
        &[0; (ProcessOutputEvent::MAX_CONTENT_LENGTH * 2) + 5],
    )
    .map(|event| event.content().len())
    .collect::<Vec<_>>();
    assert_eq!(
        event_lengths,
        [
            ProcessOutputEvent::MAX_CONTENT_LENGTH,
            ProcessOutputEvent::MAX_CONTENT_LENGTH,
            5,
        ]
    );
}