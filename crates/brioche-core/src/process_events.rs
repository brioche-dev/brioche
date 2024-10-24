//! Implementation for reading and writing the "process event" format, which
//! records events when a process runs, such as stdout and stderr, exit status,
//! and the recipe used to define the process.
//!
//! This is a bespoke binary format designed to be reasonably compact, and
//! with a structure that allows parsing from either the start or end of
//! a file. The file starts with a magic header, followed by any number
//! of events which look like this:
//!
//! ```plain
//! | marker (40 bytes)        | body (length) | marker (40 bytes)        |
//! | kind (u8) | length (u32) |               | kind (u8) | length (u32) |
//! ```
//!
//! The `marker` for an event is added both before and after the event body,
//! which allows for easily reading starting from either end of a file,
//! and efficiently reading the next or previous event at any point.

use std::{borrow::Cow, path::Path, time::Duration};

use anyhow::Context as _;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};

use crate::{
    recipe::{CompleteProcessRecipe, Meta},
    reporter::job::ProcessStream,
    sandbox::SandboxExecutionConfig,
};

pub struct ProcessEventWriter<W>
where
    W: tokio::io::AsyncWrite,
{
    writer: W,
}

impl<W> ProcessEventWriter<W>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    pub async fn new(writer: W) -> anyhow::Result<Self> {
        let mut log_writer = Self { writer };

        log_writer
            .writer
            .write_all(PROCESS_EVENT_MAGIC.as_bytes())
            .await?;

        Ok(log_writer)
    }

    pub async fn write_event(&mut self, event: &ProcessEvent<'_>) -> anyhow::Result<()> {
        // The event marker, which describes the type and length of an event
        let marker;

        // Track how many bytes we've written for the body of the event
        let mut written_length = 0;

        // Write the event marker and the body to the writer based on
        // the type of event
        match event {
            ProcessEvent::Description(description) => {
                let description_bytes = serde_json::to_vec(description)?;

                marker = ProcessEventMarker {
                    kind: ProcessEventKind::Description,
                    length: description_bytes.len(),
                };

                self.write_event_marker(marker).await?;

                written_length += self.write_bytes(&description_bytes).await?;
            }
            ProcessEvent::Spawned(event) => {
                marker = ProcessEventMarker {
                    kind: ProcessEventKind::Spawned,
                    length: 8,
                };

                self.write_event_marker(marker).await?;

                written_length += self.write_duration(event.elapsed).await?;
                written_length += self.write_u32(event.pid).await?;
            }
            ProcessEvent::Output(event) => {
                let kind = match event.stream {
                    ProcessStream::Stdout => ProcessEventKind::Stdout,
                    ProcessStream::Stderr => ProcessEventKind::Stderr,
                };

                marker = ProcessEventMarker {
                    kind,
                    length: 4 + event.content.len(),
                };

                self.write_event_marker(marker).await?;

                written_length += self.write_duration(event.elapsed).await?;
                written_length += self.write_bytes(&event.content).await?;
            }
            ProcessEvent::Exited(exited) => match exited.exit_status {
                crate::sandbox::ExitStatus::Code(code) => {
                    marker = ProcessEventMarker {
                        kind: ProcessEventKind::Exited,
                        length: 5,
                    };

                    self.write_event_marker(marker).await?;

                    written_length += self.write_duration(exited.elapsed).await?;
                    written_length += self.write_i8(code).await?;
                }
                crate::sandbox::ExitStatus::Signal(siginal) => {
                    marker = ProcessEventMarker {
                        kind: ProcessEventKind::ExitedWithSignal,
                        length: 8,
                    };

                    self.write_event_marker(marker).await?;

                    written_length += self.write_duration(exited.elapsed).await?;
                    written_length += self.write_i32(siginal).await?;
                }
            },
        }

        // Ensure that the number of bytes we wrote in the body matches
        // the length we wrote in the marker
        assert_eq!(marker.length, written_length);

        // Write a copy of the marker after the body of the event
        self.write_event_marker(marker).await?;

        Ok(())
    }

    async fn write_event_marker(&mut self, marker: ProcessEventMarker) -> anyhow::Result<()> {
        let length: u32 = marker
            .length
            .try_into()
            .context("tried to write message header, but message is too long")?;

        self.writer.write_u8(marker.kind.into()).await?;
        self.writer.write_u32(length).await?;

        Ok(())
    }

    async fn write_i8(&mut self, n: i8) -> anyhow::Result<usize> {
        self.writer.write_i8(n).await?;
        Ok(1)
    }

    async fn write_i32(&mut self, n: i32) -> anyhow::Result<usize> {
        self.writer.write_i32(n).await?;
        Ok(4)
    }

    async fn write_u32(&mut self, n: u32) -> anyhow::Result<usize> {
        self.writer.write_u32(n).await?;
        Ok(4)
    }

    async fn write_duration(&mut self, duration: Duration) -> anyhow::Result<usize> {
        let milliseconds = duration.as_millis();
        let milliseconds = std::cmp::min(milliseconds, u32::MAX as _);
        let milliseconds = milliseconds as u32;

        let length = self.write_u32(milliseconds).await?;
        Ok(length)
    }

    async fn write_bytes(&mut self, bytes: &[u8]) -> anyhow::Result<usize> {
        self.writer.write_all(bytes).await?;
        Ok(bytes.len())
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.writer.shutdown().await?;
        Ok(())
    }
}

pub struct ProcessEventReader<R>
where
    R: tokio::io::AsyncRead + Unpin,
{
    reader: R,
}

impl<R> ProcessEventReader<R>
where
    R: tokio::io::AsyncRead + Unpin,
{
    pub async fn new(mut reader: R) -> Result<Self, ProcessEventReadError> {
        let mut magic_bytes = [0u8; PROCESS_EVENT_MAGIC.len()];

        reader.read_exact(&mut magic_bytes).await?;

        let magic_bytes = bstr::BStr::new(&magic_bytes);
        if magic_bytes != PROCESS_EVENT_MAGIC {
            return Err(ProcessEventReadError::MagicDidNotMatch {
                expected: PROCESS_EVENT_MAGIC.into(),
                actual: magic_bytes.into(),
            });
        }

        Ok(Self { reader })
    }

    pub async fn read_next_event(
        &mut self,
    ) -> Result<Option<ProcessEvent<'static>>, ProcessEventReadError> {
        // Read the next marker, or return if there's no next event
        let marker = self.read_next_marker().await?;
        let Some(marker) = marker else {
            return Ok(None);
        };

        // Read the event based on its kind and length
        let event = match marker.kind {
            ProcessEventKind::Description => {
                let mut description_bytes = vec![0u8; marker.length];
                self.read_fill(&mut description_bytes).await?;
                let description = serde_json::from_slice(&description_bytes)?;

                ProcessEvent::Description(description)
            }
            ProcessEventKind::Spawned => {
                if marker.length != 8 {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                }

                let elapsed = self.read_duration().await?;
                let pid = self.read_u32().await?;

                ProcessEvent::Spawned(ProcessSpawnedEvent { elapsed, pid })
            }
            ProcessEventKind::Stdout => {
                let content_length = marker.length.checked_sub(4);
                let Some(content_length) = content_length else {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                };

                let elapsed = self.read_duration().await?;
                let mut content = vec![0u8; content_length];
                self.read_fill(&mut content).await?;

                let content = bstr::BString::new(content);
                let content = Cow::Owned(content);

                ProcessEvent::Output(ProcessOutputEvent {
                    elapsed,
                    stream: ProcessStream::Stdout,
                    content,
                })
            }
            ProcessEventKind::Stderr => {
                let content_length = marker.length.checked_sub(4);
                let Some(content_length) = content_length else {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                };

                let elapsed = self.read_duration().await?;
                let mut content = vec![0u8; content_length];
                self.read_fill(&mut content).await?;

                let content = bstr::BString::new(content);
                let content = Cow::Owned(content);

                ProcessEvent::Output(ProcessOutputEvent {
                    elapsed,
                    stream: ProcessStream::Stderr,
                    content,
                })
            }
            ProcessEventKind::Exited => {
                if marker.length != 5 {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                }

                let elapsed = self.read_duration().await?;
                let code = self.read_i8().await?;

                ProcessEvent::Exited(ProcessExitedEvent {
                    elapsed,
                    exit_status: crate::sandbox::ExitStatus::Code(code),
                })
            }
            ProcessEventKind::ExitedWithSignal => {
                if marker.length != 8 {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                }

                let elapsed = self.read_duration().await?;
                let signal = self.read_i32().await?;

                ProcessEvent::Exited(ProcessExitedEvent {
                    elapsed,
                    exit_status: crate::sandbox::ExitStatus::Signal(signal),
                })
            }
        };

        // Each event ends with a copy of its marker, so read the next
        // marker and ensure it matches the start marker that we read
        let end_marker = self.read_next_marker().await?;
        let Some(end_marker) = end_marker else {
            return Err(ProcessEventReadError::CutOff);
        };

        if marker != end_marker {
            return Err(ProcessEventReadError::EventMarkerDidNotMatch {
                start_marker: marker,
                end_marker,
            });
        }

        Ok(Some(event))
    }

    pub async fn read_previous_event(
        &mut self,
    ) -> Result<Option<ProcessEvent<'static>>, ProcessEventReadError>
    where
        R: tokio::io::AsyncSeek,
    {
        // Read the previous marker by seeking backwards. If the current
        // position is right after the magic bytes, exit early because there
        // are no earlier events.
        let previous_marker_result = self.read_previous_marker().await?;
        let Some((end_marker, end_marker_start_pos)) = previous_marker_result else {
            return Ok(None);
        };

        let process_event_marker_length: u64 = PROCESS_EVENT_MARKER_LENGTH.try_into().unwrap();
        let process_event_magic_length: u64 = PROCESS_EVENT_MAGIC.len().try_into().unwrap();

        let event_length: u64 =
            end_marker
                .length
                .try_into()
                .map_err(|_| ProcessEventReadError::LengthOutOfRange {
                    length: end_marker.length as _,
                })?;

        // Get the position of the start marker for the event by subtracting
        // the marker length and the event's length
        let start_marker_start_pos = end_marker_start_pos
            .saturating_sub(process_event_marker_length)
            .saturating_sub(event_length);

        // Ensure the position is in-bounds (not within the magic bytes)
        if start_marker_start_pos < process_event_magic_length {
            return Err(ProcessEventReadError::MisalignedEvent);
        }

        // Seek to the position of the start marker for the event
        self.reader
            .seek(std::io::SeekFrom::Start(start_marker_start_pos))
            .await?;

        // Read the event (something went terribly wrong if this returned
        // `None`)
        let event = self.read_next_event().await?;
        let Some(event) = event else {
            return Err(ProcessEventReadError::CutOff);
        };

        // Seek back to the position of the start marker for the event. From
        // the start, the seek position is one event earlier than it was
        self.reader
            .seek(std::io::SeekFrom::Start(start_marker_start_pos))
            .await?;

        Ok(Some(event))
    }

    pub async fn seek_to_end(&mut self) -> std::io::Result<()>
    where
        R: tokio::io::AsyncSeek,
    {
        self.reader.seek(std::io::SeekFrom::End(0)).await?;
        Ok(())
    }

    async fn read_next_marker(
        &mut self,
    ) -> Result<Option<ProcessEventMarker>, ProcessEventReadError> {
        // Read enough bytes for the next marker
        let mut marker_bytes = [0u8; PROCESS_EVENT_MARKER_LENGTH];
        let marker_bytes_len = self.try_read_fill(&mut marker_bytes).await?;
        let marker_bytes = &marker_bytes[0..marker_bytes_len];

        // If we didn't read anything, then we're at the end of the reader,
        // so return `None`
        if marker_bytes.is_empty() {
            return Ok(None);
        }

        // Parse the marker
        let start_marker = ProcessEventMarker::from_bytes(marker_bytes)?;
        Ok(Some(start_marker))
    }

    async fn read_previous_marker(
        &mut self,
    ) -> Result<Option<(ProcessEventMarker, u64)>, ProcessEventReadError>
    where
        R: tokio::io::AsyncSeek,
    {
        let current_pos = self.reader.stream_position().await?;
        let initial_start_pos: u64 = PROCESS_EVENT_MAGIC.len().try_into().unwrap();

        // If we're right after the magic bytes, then there's no previous
        // marker to read, so return `None`
        if current_pos == initial_start_pos {
            return Ok(None);
        }

        // Calculate where the start of the previous marker is, and ensure
        // we don't end up before the magic bytes
        let marker_start_pos =
            current_pos.saturating_sub(PROCESS_EVENT_MARKER_LENGTH.try_into().unwrap());
        if marker_start_pos <= initial_start_pos {
            return Err(ProcessEventReadError::MisalignedEvent);
        }

        // Seek to the start of the marker, then read it
        self.reader
            .seek(std::io::SeekFrom::Start(marker_start_pos))
            .await?;

        let marker = self.read_next_marker().await?;

        let Some(marker) = marker else {
            return Err(ProcessEventReadError::CutOff);
        };

        Ok(Some((marker, marker_start_pos)))
    }

    async fn read_u32(&mut self) -> Result<u32, ProcessEventReadError> {
        let mut bytes = [0; 4];
        self.read_fill(&mut bytes).await?;

        Ok(u32::from_be_bytes(bytes))
    }

    async fn read_i8(&mut self) -> Result<i8, ProcessEventReadError> {
        let mut bytes = [0; 1];
        self.read_fill(&mut bytes).await?;

        Ok(i8::from_be_bytes(bytes))
    }

    async fn read_i32(&mut self) -> Result<i32, ProcessEventReadError> {
        let mut bytes = [0; 4];
        self.read_fill(&mut bytes).await?;

        Ok(i32::from_be_bytes(bytes))
    }

    async fn read_duration(&mut self) -> Result<Duration, ProcessEventReadError> {
        let milliseconds = self.read_u32().await?;
        Ok(Duration::from_millis(milliseconds.into()))
    }

    async fn try_read_fill(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total_bytes_read = 0;

        loop {
            buf = &mut buf[total_bytes_read..];
            if buf.is_empty() {
                break;
            }

            let bytes_read = self.reader.read(buf).await?;
            total_bytes_read += bytes_read;

            if bytes_read == 0 {
                break;
            }
        }

        Ok(total_bytes_read)
    }

    async fn read_fill(&mut self, buf: &mut [u8]) -> Result<(), ProcessEventReadError> {
        let read_len = self.try_read_fill(buf).await?;
        if read_len != buf.len() {
            return Err(ProcessEventReadError::CutOff);
        }

        Ok(())
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, num_enum::TryFromPrimitive, num_enum::IntoPrimitive,
)]
#[repr(u8)]
enum ProcessEventKind {
    Description = 1,
    Spawned = 2,
    Stdout = 3,
    Stderr = 4,
    Exited = 5,
    ExitedWithSignal = 6,
}

const PROCESS_EVENT_MAGIC: &str = "brioche_process_events v0       ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessEventMarker {
    kind: ProcessEventKind,
    length: usize,
}

impl ProcessEventMarker {
    fn from_bytes(bytes: &[u8]) -> Result<Self, ProcessEventReadError> {
        assert!(bytes.len() <= PROCESS_EVENT_MARKER_LENGTH);

        if bytes.len() < PROCESS_EVENT_MARKER_LENGTH {
            return Err(ProcessEventReadError::CutOff);
        }

        let kind = bytes[0];
        let kind = ProcessEventKind::try_from(kind)
            .map_err(|_| ProcessEventReadError::UnknownEvent { event_kind: kind })?;

        let length = &bytes[1..];
        let length: [u8; 4] = length.try_into().unwrap();
        let length = u32::from_be_bytes(length);
        let length: usize = length
            .try_into()
            .map_err(|_| ProcessEventReadError::LengthOutOfRange { length })?;

        Ok(Self { kind, length })
    }
}

const PROCESS_EVENT_MARKER_LENGTH: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEvent<'a> {
    Description(ProcessEventDescription<'a>),
    Spawned(ProcessSpawnedEvent),
    Output(ProcessOutputEvent<'a>),
    Exited(ProcessExitedEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessEventDescription<'a> {
    pub recipe: Cow<'a, CompleteProcessRecipe>,
    pub meta: Cow<'a, Meta>,
    pub sandbox_config: Cow<'a, SandboxExecutionConfig>,
    pub root_dir: Cow<'a, Path>,
    pub output_dir: Cow<'a, Path>,
    pub created_at: jiff::Zoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessSpawnedEvent {
    pub elapsed: Duration,
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutputEvent<'a> {
    pub elapsed: Duration,
    pub stream: ProcessStream,
    pub content: Cow<'a, bstr::BStr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessExitedEvent {
    pub elapsed: Duration,
    pub exit_status: crate::sandbox::ExitStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessEventReadError {
    #[error("process event file magic did not match (expected {expected:?}, but got {actual:?})")]
    MagicDidNotMatch {
        expected: bstr::BString,
        actual: bstr::BString,
    },

    #[error("process event file ended abruptly")]
    CutOff,

    #[error("process event file appears to be corrupted: start marker {start_marker:?} did not match end marker {end_marker:?}")]
    EventMarkerDidNotMatch {
        start_marker: ProcessEventMarker,
        end_marker: ProcessEventMarker,
    },

    #[error("process event file appears to be corrupted: unknown event kind {event_kind:?}")]
    UnknownEvent { event_kind: u8 },

    #[error("length {length} out of range")]
    LengthOutOfRange { length: u32 },

    #[error("process event file appears to be corrupted: invalid length of {} for event {:?}", marker.length, marker.kind)]
    InvalidEventLength { marker: ProcessEventMarker },

    #[error("tried to read event marker, but tried to seek outside of the file")]
    MisalignedEvent,

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    SerdeError(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, time::Duration};

    use jiff::Zoned;

    use crate::{
        recipe::{CompleteProcessRecipe, CompleteProcessTemplate, Meta},
        reporter::job::ProcessStream,
        sandbox::{ExitStatus, SandboxExecutionConfig, SandboxPath, SandboxPathOptions},
    };

    use super::{
        ProcessEvent, ProcessEventDescription, ProcessEventReader, ProcessEventWriter,
        ProcessExitedEvent, ProcessOutputEvent, ProcessSpawnedEvent,
    };

    pub fn default_complete_process() -> CompleteProcessRecipe {
        CompleteProcessRecipe {
            command: CompleteProcessTemplate { components: vec![] },
            args: vec![],
            env: Default::default(),
            work_dir: Default::default(),
            output_scaffold: None,
            platform: crate::platform::current_platform(),
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
                        mode: crate::sandbox::HostPathMode::Read,
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
            ProcessEvent::Output(ProcessOutputEvent {
                elapsed: Duration::from_secs(2),
                stream: ProcessStream::Stdout,
                content: Cow::Owned("foo".into()),
            }),
            ProcessEvent::Output(ProcessOutputEvent {
                elapsed: Duration::from_secs(3),
                stream: ProcessStream::Stderr,
                content: Cow::Owned("bar".into()),
            }),
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
            ProcessEvent::Output(ProcessOutputEvent {
                elapsed: Duration::from_secs(2),
                stream: ProcessStream::Stdout,
                content: Cow::Owned("foo".into()),
            }),
            ProcessEvent::Output(ProcessOutputEvent {
                elapsed: Duration::from_secs(3),
                stream: ProcessStream::Stderr,
                content: Cow::Owned("bar".into()),
            }),
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

        assert_eq!(buffer, super::PROCESS_EVENT_MAGIC.as_bytes());

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
}
