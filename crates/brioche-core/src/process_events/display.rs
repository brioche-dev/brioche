use std::collections::VecDeque;

use bstr::ByteSlice;

use crate::{reporter::job::ProcessStream, utils::output_buffer::OutputBuffer};

use super::{reader::ProcessEventReader, ProcessEvent};

pub struct DisplayEventsOptions {
    pub reverse: bool,
    pub limit: Option<usize>,
    pub follow_events: Option<std::sync::mpsc::Receiver<anyhow::Result<()>>>,
}

pub fn display_events<R>(
    reader: &mut ProcessEventReader<R>,
    options: DisplayEventsOptions,
) -> anyhow::Result<()>
where
    R: std::io::Read + std::io::Seek,
{
    let mut initial_events = VecDeque::new();
    let mut spawned_at = std::time::Duration::ZERO;

    // Read the first two events first to look for the "spawned" event so
    // we can get adjust the "elapsed" durations relative to the spawn time
    // instead of the initial time. If reading forward, we queue these
    // events to avoid seeking back to the beginning (if reading reverse,
    // then those events will be read again later on).
    for _ in 0..2 {
        let event = reader.read_next_event();

        if let Ok(Some(ProcessEvent::Spawned(event))) = &event {
            spawned_at = event.elapsed;
        }

        if !options.reverse {
            initial_events.push_back(event);
        }
    }

    if options.reverse {
        reader.seek_to_end()?;
    }

    let mut output = OutputBuffer::<ProcessStream>::with_unlimited_capacity();
    let mut limit = options.limit;
    let mut reached_exited_event = false;
    loop {
        if limit == Some(0) {
            break;
        }

        // Try reading the next event (or previous if we're reading backwards)
        let pos = reader.pos();
        let event = if options.reverse {
            reader.read_previous_event()
        } else if let Some(event) = initial_events.pop_front() {
            event
        } else {
            reader.read_next_event()
        };

        // Get the event, or break or retry depending on the configured
        // options
        let event = match event {
            Ok(Some(event)) => event,
            Ok(None) => {
                // No next event, meaning we reached EOF

                // If we've already seen the "exited" event, then we're done
                if reached_exited_event {
                    break;
                }

                if let Some(follow_events) = &options.follow_events {
                    // If `--follow` was used, wait for the next notification,
                    // then retry
                    follow_events.recv()??;
                    continue;
                } else {
                    // We're at end of file, so we're done
                    break;
                }
            }
            Err(error) if error.is_unexpected_eof() => {
                // We got an "unexpected EOF" error

                if let Some(follow_events) = &options.follow_events {
                    // If `--follow` was used, wait for the next notification
                    follow_events.recv()??;

                    // Seek to the position we were at before trying to read
                    // the next event
                    reader.seek_to_pos(pos)?;

                    // Retry the read
                    continue;
                } else {
                    // Otherwise, just bubble up the error
                    return Err(error.into());
                }
            }
            Err(error) => {
                // For other kinds of errors, bubble it up
                return Err(error.into());
            }
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
                reached_exited_event = true;
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
