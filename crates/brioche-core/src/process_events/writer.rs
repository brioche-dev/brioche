use std::time::Duration;

use anyhow::Context as _;
use tokio::io::AsyncWriteExt as _;

use crate::reporter::job::ProcessStream;

use super::{PROCESS_EVENT_MAGIC, ProcessEvent, ProcessEventKind, ProcessEventMarker};

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

    pub async fn write_event(&mut self, event: &ProcessEvent) -> anyhow::Result<()> {
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
            ProcessEvent::Exited(exited) => match &exited.exit_status {
                crate::sandbox::ExitStatus::Code(code) => {
                    marker = ProcessEventMarker {
                        kind: ProcessEventKind::Exited,
                        length: 8,
                    };

                    self.write_event_marker(marker).await?;

                    written_length += self.write_duration(exited.elapsed).await?;
                    written_length += self.write_i32(*code).await?;
                }
                crate::sandbox::ExitStatus::Signal(siginal) => {
                    marker = ProcessEventMarker {
                        kind: ProcessEventKind::ExitedWithSignal,
                        length: 8,
                    };

                    self.write_event_marker(marker).await?;

                    written_length += self.write_duration(exited.elapsed).await?;
                    written_length += self.write_i32(*siginal).await?;
                }
                crate::sandbox::ExitStatus::Other { message } => {
                    marker = ProcessEventMarker {
                        kind: ProcessEventKind::ExitedWithMessage,
                        length: 4 + message.len(),
                    };

                    self.write_event_marker(marker).await?;

                    written_length += self.write_duration(exited.elapsed).await?;
                    written_length += self.write_bytes(message.as_bytes()).await?;
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

    async fn write_i32(&mut self, n: i32) -> anyhow::Result<usize> {
        self.writer.write_i32(n).await?;
        Ok(4)
    }

    async fn write_u32(&mut self, n: u32) -> anyhow::Result<usize> {
        self.writer.write_u32(n).await?;
        Ok(4)
    }

    #[expect(clippy::cast_possible_truncation)]
    async fn write_duration(&mut self, duration: Duration) -> anyhow::Result<usize> {
        let milliseconds = duration.as_millis();
        let milliseconds = std::cmp::min(milliseconds, u32::MAX.into());
        let milliseconds = milliseconds as u32;

        let length = self.write_u32(milliseconds).await?;
        Ok(length)
    }

    async fn write_bytes(&mut self, bytes: &[u8]) -> anyhow::Result<usize> {
        self.writer.write_all(bytes).await?;
        Ok(bytes.len())
    }

    pub const fn inner_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.writer.shutdown().await?;
        Ok(())
    }
}
