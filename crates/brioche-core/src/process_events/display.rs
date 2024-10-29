use std::collections::VecDeque;

use bstr::ByteSlice;

use crate::{reporter::job::ProcessStream, utils::output_buffer::OutputBuffer};

use super::{reader::ProcessEventReader, ProcessEvent};

pub struct DisplayEventsOptions {
    pub reverse: bool,
    pub limit: Option<usize>,
}

pub async fn display_events<R>(
    reader: &mut ProcessEventReader<R>,
    options: DisplayEventsOptions,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin,
{
    let mut initial_events = VecDeque::new();
    let mut spawned_at = std::time::Duration::ZERO;

    // Read the first two events first to look for the "spawned" event so
    // we can get adjust the "elapsed" durations relative to the spawn time
    // instead of the initial time. If reading forward, we queue these
    // events to avoid seeking back to the beginning (if reading reverse,
    // then those events will be read again later on).
    for _ in 0..2 {
        let event = reader.read_next_event().await;

        if let Ok(Some(ProcessEvent::Spawned(event))) = &event {
            spawned_at = event.elapsed;
        }

        if !options.reverse {
            initial_events.push_back(event);
        }
    }

    if options.reverse {
        reader.seek_to_end().await?;
    }

    let mut output = OutputBuffer::<ProcessStream>::with_unlimited_capacity();
    let mut limit = options.limit;
    loop {
        if limit == Some(0) {
            break;
        }

        let event = if options.reverse {
            reader.read_previous_event().await?
        } else if let Some(event) = initial_events.pop_front() {
            event?
        } else {
            reader.read_next_event().await?
        };

        let Some(event) = event else {
            break;
        };

        match event {
            ProcessEvent::Description(description) => {
                if options.reverse {
                    println!();
                }

                println!("process stack trace:");
                let stack_frames = description.meta.source.as_deref().unwrap_or_default();

                for stack_frame in stack_frames {
                    let Some(file_name) = &stack_frame.file_name else {
                        println!("- [unknown]");
                        continue;
                    };

                    let Some(line_number) = stack_frame.line_number else {
                        println!("- {}", file_name);
                        continue;
                    };

                    let Some(column_number) = stack_frame.column_number else {
                        println!("- {}:{}", file_name, line_number);
                        continue;
                    };

                    println!("- {}:{}:{}", file_name, line_number, column_number);
                }

                if stack_frames.is_empty() {
                    println!("- [empty]");
                }

                if !options.reverse {
                    println!();
                }

                if let Some(ref mut limit) = limit {
                    *limit = limit.saturating_sub(1);
                }
            }
            ProcessEvent::Spawned(event) => {
                // This should (normally) show as an elapsed duration of zero,
                // since we tried to find the "spawned" event as the basis
                // for durations
                let elapsed = event.elapsed.saturating_sub(spawned_at);
                let elapsed = crate::utils::DisplayDuration(elapsed);

                let preparation_elapsed = crate::utils::DisplayDuration(event.elapsed);

                println!(
                    "[{elapsed}] [spawned process with pid {}, preparation took {preparation_elapsed}]",
                    event.pid
                );

                if let Some(ref mut limit) = limit {
                    *limit = limit.saturating_sub(1);
                }
            }
            ProcessEvent::Output(event) => {
                if options.reverse {
                    output.prepend(event.stream, &*event.content);
                } else {
                    output.append(event.stream, &*event.content);
                }

                loop {
                    let content = if options.reverse {
                        output.pop_back()
                    } else {
                        output.pop_front()
                    };
                    let Some((_, content)) = content else {
                        break;
                    };

                    let elapsed = event.elapsed.saturating_sub(spawned_at);
                    let elapsed = crate::utils::DisplayDuration(elapsed);

                    for line in content.lines() {
                        let line = bstr::BStr::new(line);
                        println!("[{elapsed}] {line}");

                        if let Some(ref mut limit) = limit {
                            *limit = limit.saturating_sub(1);
                            if *limit == 0 {
                                break;
                            }
                        }
                    }
                }
            }
            ProcessEvent::Exited(event) => {
                let elapsed = event.elapsed.saturating_sub(spawned_at);
                let elapsed = crate::utils::DisplayDuration(elapsed);

                match event.exit_status {
                    crate::sandbox::ExitStatus::Code(code) => {
                        println!("[{elapsed}] [process exited with code {code}]");
                    }
                    crate::sandbox::ExitStatus::Signal(signal) => {
                        println!("[{elapsed}] [process exited with signal {signal}]");
                    }
                }

                if let Some(ref mut limit) = limit {
                    *limit = limit.saturating_sub(1);
                }
            }
        };
    }

    Ok(())
}
