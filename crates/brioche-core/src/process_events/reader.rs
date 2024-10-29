use std::{borrow::Cow, time::Duration};

use crate::reporter::job::ProcessStream;

use super::{
    ProcessEvent, ProcessEventKind, ProcessEventMarker, ProcessEventReadError, ProcessExitedEvent,
    ProcessOutputEvent, ProcessSpawnedEvent, PROCESS_EVENT_MAGIC, PROCESS_EVENT_MARKER_LENGTH,
};

pub struct ProcessEventReader<R>
where
    R: std::io::Read,
{
    reader: R,
}

impl<R> ProcessEventReader<R>
where
    R: std::io::Read,
{
    pub fn new(mut reader: R) -> Result<Self, ProcessEventReadError> {
        let mut magic_bytes = [0u8; PROCESS_EVENT_MAGIC.len()];

        reader.read_exact(&mut magic_bytes)?;

        let magic_bytes = bstr::BStr::new(&magic_bytes);
        if magic_bytes != PROCESS_EVENT_MAGIC {
            return Err(ProcessEventReadError::MagicDidNotMatch {
                expected: PROCESS_EVENT_MAGIC.into(),
                actual: magic_bytes.into(),
            });
        }

        Ok(Self { reader })
    }

    pub fn read_next_event(
        &mut self,
    ) -> Result<Option<ProcessEvent<'static>>, ProcessEventReadError> {
        // Read the next marker, or return if there's no next event
        let marker = self.read_next_marker()?;
        let Some(marker) = marker else {
            return Ok(None);
        };

        // Read the event based on its kind and length
        let event = match marker.kind {
            ProcessEventKind::Description => {
                let mut description_bytes = vec![0u8; marker.length];
                self.read_fill(&mut description_bytes)?;
                let description = serde_json::from_slice(&description_bytes)?;

                ProcessEvent::Description(description)
            }
            ProcessEventKind::Spawned => {
                if marker.length != 8 {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                }

                let elapsed = self.read_duration()?;
                let pid = self.read_u32()?;

                ProcessEvent::Spawned(ProcessSpawnedEvent { elapsed, pid })
            }
            ProcessEventKind::Stdout => {
                let content_length = marker.length.checked_sub(4);
                let Some(content_length) = content_length else {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                };

                // Validate the length before allocating and reading
                ProcessOutputEvent::validate_length(content_length)?;

                let elapsed = self.read_duration()?;
                let mut content = vec![0u8; content_length];
                self.read_fill(&mut content)?;

                let content = bstr::BString::new(content);
                let content = Cow::Owned(content);

                let event = ProcessOutputEvent::new(elapsed, ProcessStream::Stdout, content)?;
                ProcessEvent::Output(event)
            }
            ProcessEventKind::Stderr => {
                let content_length = marker.length.checked_sub(4);
                let Some(content_length) = content_length else {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                };

                // Validate the length before allocating and reading
                ProcessOutputEvent::validate_length(content_length)?;

                let elapsed = self.read_duration()?;
                let mut content = vec![0u8; content_length];
                self.read_fill(&mut content)?;

                let content = bstr::BString::new(content);
                let content = Cow::Owned(content);

                let event = ProcessOutputEvent::new(elapsed, ProcessStream::Stderr, content)?;
                ProcessEvent::Output(event)
            }
            ProcessEventKind::Exited => {
                if marker.length != 5 {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                }

                let elapsed = self.read_duration()?;
                let code = self.read_i8()?;

                ProcessEvent::Exited(ProcessExitedEvent {
                    elapsed,
                    exit_status: crate::sandbox::ExitStatus::Code(code),
                })
            }
            ProcessEventKind::ExitedWithSignal => {
                if marker.length != 8 {
                    return Err(ProcessEventReadError::InvalidEventLength { marker });
                }

                let elapsed = self.read_duration()?;
                let signal = self.read_i32()?;

                ProcessEvent::Exited(ProcessExitedEvent {
                    elapsed,
                    exit_status: crate::sandbox::ExitStatus::Signal(signal),
                })
            }
        };

        // Each event ends with a copy of its marker, so read the next
        // marker and ensure it matches the start marker that we read
        let end_marker = self.read_next_marker()?;
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

    pub fn read_previous_event(
        &mut self,
    ) -> Result<Option<ProcessEvent<'static>>, ProcessEventReadError>
    where
        R: std::io::Seek,
    {
        // Read the previous marker by seeking backwards. If the current
        // position is right after the magic bytes, exit early because there
        // are no earlier events.
        let previous_marker_result = self.read_previous_marker()?;
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
            .seek(std::io::SeekFrom::Start(start_marker_start_pos))?;

        // Read the event (something went terribly wrong if this returned
        // `None`)
        let event = self.read_next_event()?;
        let Some(event) = event else {
            return Err(ProcessEventReadError::CutOff);
        };

        // Seek back to the position of the start marker for the event. From
        // the start, the seek position is one event earlier than it was
        self.reader
            .seek(std::io::SeekFrom::Start(start_marker_start_pos))?;

        Ok(Some(event))
    }

    pub fn skip_next_event(&mut self) -> Result<bool, ProcessEventReadError>
    where
        R: std::io::Seek,
    {
        // Read the next marker, or return if there's no next event
        let marker = self.read_next_marker()?;
        let Some(marker) = marker else {
            return Ok(false);
        };

        // Skip past the next event and the end marker for the event
        let next_event_offset = marker
            .length
            .checked_add(PROCESS_EVENT_MARKER_LENGTH)
            .and_then(|offset| offset.try_into().ok())
            .ok_or_else(|| ProcessEventReadError::LengthOutOfRange {
                length: marker.length as _,
            })?;

        self.reader
            .seek(std::io::SeekFrom::Current(next_event_offset))?;

        Ok(true)
    }

    pub fn skip_previous_event(&mut self) -> Result<bool, ProcessEventReadError>
    where
        R: std::io::Seek,
    {
        // Read the previous marker, or return if there's no previous event
        let previous_marker_result = self.read_previous_marker()?;
        let Some((marker, marker_start_offset)) = previous_marker_result else {
            return Ok(false);
        };

        // Skip before the previous event and the start marker for the event
        let previous_event_offset = marker
            .length
            .checked_add(PROCESS_EVENT_MARKER_LENGTH)
            .and_then(|offset| offset.try_into().ok())
            .ok_or_else(|| ProcessEventReadError::LengthOutOfRange {
                length: marker.length as _,
            })?;

        self.reader.seek(std::io::SeekFrom::Start(
            marker_start_offset.saturating_sub(previous_event_offset),
        ))?;

        Ok(true)
    }

    pub fn seek_to_end(&mut self) -> Result<(), ProcessEventReadError>
    where
        R: std::io::Seek,
    {
        self.reader.seek(std::io::SeekFrom::End(0))?;
        Ok(())
    }

    fn read_next_marker(&mut self) -> Result<Option<ProcessEventMarker>, ProcessEventReadError> {
        // Read enough bytes for the next marker
        let mut marker_bytes = [0u8; PROCESS_EVENT_MARKER_LENGTH];
        let marker_bytes_len = self.try_read_fill(&mut marker_bytes)?;
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

    fn read_previous_marker(
        &mut self,
    ) -> Result<Option<(ProcessEventMarker, u64)>, ProcessEventReadError>
    where
        R: std::io::Seek,
    {
        let current_pos = self.reader.stream_position()?;
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
            .seek(std::io::SeekFrom::Start(marker_start_pos))?;

        let marker = self.read_next_marker()?;

        let Some(marker) = marker else {
            return Err(ProcessEventReadError::CutOff);
        };

        Ok(Some((marker, marker_start_pos)))
    }

    fn read_u32(&mut self) -> Result<u32, ProcessEventReadError> {
        let mut bytes = [0; 4];
        self.read_fill(&mut bytes)?;

        Ok(u32::from_be_bytes(bytes))
    }

    fn read_i8(&mut self) -> Result<i8, ProcessEventReadError> {
        let mut bytes = [0; 1];
        self.read_fill(&mut bytes)?;

        Ok(i8::from_be_bytes(bytes))
    }

    fn read_i32(&mut self) -> Result<i32, ProcessEventReadError> {
        let mut bytes = [0; 4];
        self.read_fill(&mut bytes)?;

        Ok(i32::from_be_bytes(bytes))
    }

    fn read_duration(&mut self) -> Result<Duration, ProcessEventReadError> {
        let milliseconds = self.read_u32()?;
        Ok(Duration::from_millis(milliseconds.into()))
    }

    fn try_read_fill(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total_bytes_read = 0;

        loop {
            let buf = &mut buf[total_bytes_read..];
            if buf.is_empty() {
                break;
            }

            let bytes_read = self.reader.read(buf)?;
            total_bytes_read += bytes_read;

            if bytes_read == 0 {
                break;
            }
        }

        Ok(total_bytes_read)
    }

    fn read_fill(&mut self, buf: &mut [u8]) -> Result<(), ProcessEventReadError> {
        let read_len = self.try_read_fill(buf)?;
        if read_len != buf.len() {
            return Err(ProcessEventReadError::CutOff);
        }

        Ok(())
    }
}
