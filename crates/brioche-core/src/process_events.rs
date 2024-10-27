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

use crate::{
    recipe::{CompleteProcessRecipe, Meta},
    reporter::job::ProcessStream,
    sandbox::SandboxExecutionConfig,
};

pub mod reader;
pub mod writer;

pub fn create_process_output_events(
    elapsed: Duration,
    stream: ProcessStream,
    content: &[u8],
) -> impl Iterator<Item = ProcessOutputEvent<'_>> {
    content
        .chunks(ProcessOutputEvent::MAX_CONTENT_LENGTH)
        .map(move |chunk| ProcessOutputEvent {
            elapsed,
            stream,
            content: Cow::Borrowed(bstr::BStr::new(chunk)),
        })
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

pub const PROCESS_EVENT_MAGIC: &str = "brioche_process_events v0       ";

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
    content: Cow<'a, bstr::BStr>,
}

impl<'a> ProcessOutputEvent<'a> {
    pub const MAX_CONTENT_LENGTH: usize = 1024 * 1024;

    fn validate_length(length: usize) -> Result<(), CreateProcessOutputEventError> {
        if length == 0 {
            return Err(CreateProcessOutputEventError::EmptyContent);
        } else if length > Self::MAX_CONTENT_LENGTH {
            return Err(CreateProcessOutputEventError::ContentTooLong {
                max_length: Self::MAX_CONTENT_LENGTH,
                actual_length: length,
            });
        }

        Ok(())
    }

    pub fn new(
        elapsed: Duration,
        stream: ProcessStream,
        content: Cow<'a, bstr::BStr>,
    ) -> Result<Self, CreateProcessOutputEventError> {
        Self::validate_length(content.len())?;

        Ok(Self {
            elapsed,
            stream,
            content,
        })
    }

    pub fn content(&self) -> &bstr::BStr {
        bstr::BStr::new(&**self.content)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessExitedEvent {
    pub elapsed: Duration,
    pub exit_status: crate::sandbox::ExitStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateProcessOutputEventError {
    #[error("output content is empty")]
    EmptyContent,

    #[error("output content is too long (max length is {max_length}, got {actual_length})")]
    ContentTooLong {
        max_length: usize,
        actual_length: usize,
    },
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

    #[error(transparent)]
    InvalidProcessOutput(#[from] CreateProcessOutputEventError),

    #[error("tried to read event marker, but tried to seek outside of the file")]
    MisalignedEvent,

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    SerdeError(#[from] serde_json::Error),
}
