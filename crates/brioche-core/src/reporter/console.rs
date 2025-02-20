use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{atomic::AtomicUsize, Arc},
};

use bstr::ByteSlice;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithHttpConfig;
use superconsole::style::Stylize;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer as _};

use crate::utils::{output_buffer::OutputBuffer, DisplayDuration};

use super::{
    job::{Job, NewJob, ProcessStatus, ProcessStream, UpdateJob},
    tracing_debug_filter, tracing_output_filter, tracing_root_filter, JobId, ReportEvent, Reporter,
    ReporterGuard,
};

#[derive(Debug, Clone, Copy)]
pub enum ConsoleReporterKind {
    Auto,
    SuperConsole,
    Plain,
    PlainReduced,
}

// Render at 30 fps
const RENDER_RATE: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000 / 30);

// Minimum amount of time to sleep before rendering next frame, even if
// the next frame will be late
const MIN_RENDER_WAIT: std::time::Duration = std::time::Duration::from_millis(1);

pub fn start_console_reporter(
    kind: ConsoleReporterKind,
) -> anyhow::Result<(Reporter, ReporterGuard)> {
    let queued_lines = Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let brioche_otel_enabled = matches!(
        std::env::var("BRIOCHE_ENABLE_OTEL").as_deref(),
        Ok("1") | Ok("true")
    );

    let start = std::time::Instant::now();

    let reporter = Reporter {
        start,
        num_jobs: Arc::new(AtomicUsize::new(0)),
        tx: tx.clone(),
    };

    std::thread::spawn({
        let queued_lines = queued_lines.clone();
        move || {
            let superconsole = match kind {
                ConsoleReporterKind::Auto => superconsole::SuperConsole::new(),
                ConsoleReporterKind::SuperConsole => Some(superconsole::SuperConsole::forced_new(
                    superconsole::Dimensions {
                        width: 80,
                        height: 24,
                    },
                )),
                ConsoleReporterKind::Plain | ConsoleReporterKind::PlainReduced => None,
            };

            let jobs = HashMap::new();
            let job_outputs = OutputBuffer::with_max_capacity(1024 * 1024);
            let mut console = match (superconsole, kind) {
                (Some(console), _) => {
                    let root = JobsComponent {
                        start,
                        jobs,
                        job_outputs,
                    };
                    ConsoleReporter::SuperConsole { console, root }
                }
                (_, ConsoleReporterKind::SuperConsole) => {
                    unreachable!();
                }
                (_, ConsoleReporterKind::Auto | ConsoleReporterKind::Plain) => {
                    ConsoleReporter::Plain {
                        jobs,
                        job_outputs: Some(job_outputs),
                    }
                }
                (_, ConsoleReporterKind::PlainReduced) => ConsoleReporter::Plain {
                    jobs,
                    job_outputs: None,
                },
            };

            let mut last_render: Option<std::time::Instant> = None;
            let mut running = true;
            while running {
                // Sleep long enough to try and hit our target render rate
                if let Some(last_render) = last_render {
                    let elapsed_since_last_render = last_render.elapsed();
                    let wait_until_next_render =
                        RENDER_RATE.saturating_sub(elapsed_since_last_render);
                    let wait_until_next_render =
                        wait_until_next_render.clamp(MIN_RENDER_WAIT, RENDER_RATE);

                    std::thread::sleep(wait_until_next_render);
                };

                let _ = console.render();

                last_render = Some(std::time::Instant::now());

                while let Ok(event) = rx.try_recv() {
                    match event {
                        ReportEvent::Emit { lines } => {
                            console.emit(lines);
                        }
                        ReportEvent::AddJob { id, job } => {
                            console.add_job(id, job);
                        }
                        ReportEvent::UpdateJobState { id, update } => {
                            console.update_job(id, update);
                        }
                        ReportEvent::Shutdown => {
                            running = false;
                        }
                    }
                }
                let mut queued_lines = queued_lines.blocking_write();
                for lines in queued_lines.drain(..) {
                    console.emit(lines);
                }
            }

            let _ = console.finalize();
            let _ = shutdown_tx.send(());
        }
    });

    let guard;
    let opentelemetry_layer;

    if brioche_otel_enabled {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_http_client(reqwest::Client::new())
            .build()?;
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name("brioche")
                    .with_attribute(opentelemetry::KeyValue::new(
                        opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
                        env!("CARGO_PKG_VERSION"),
                    ))
                    .build(),
            )
            .build();

        opentelemetry_layer = Some(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer("brioche"))
                .with_filter(tracing_debug_filter()),
        );
        guard = ReporterGuard {
            tx,
            shutdown_rx: Some(shutdown_rx),
            opentelemetry_tracer_provider: Some(provider),
        };
    } else {
        opentelemetry_layer = None;
        guard = ReporterGuard {
            tx,
            shutdown_rx: Some(shutdown_rx),
            opentelemetry_tracer_provider: None,
        };
    };

    let log_file_layer = match std::env::var_os("BRIOCHE_LOG_OUTPUT") {
        Some(debug_output_path) => {
            let debug_output =
                std::fs::File::create(debug_output_path).expect("failed to open debug output path");
            Some(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(debug_output)
                    .with_timer(tracing_subscriber::fmt::time::uptime())
                    .with_filter(tracing_debug_filter()),
            )
        }
        _ => None,
    };

    let tracing_console_layer =
        std::env::var_os("BRIOCHE_CONSOLE").map(|_| console_subscriber::spawn());

    // HACK: Add a filter to the subscriber to remove debug logs that we
    // shouldn't see if no other layer needs them. This is a workaround for
    // this issue: https://github.com/tokio-rs/tracing/issues/2448
    let root_filter = match (
        &log_file_layer,
        &opentelemetry_layer,
        &tracing_console_layer,
    ) {
        (None, None, None) => Some(tracing_output_filter()),
        (_, _, Some(_)) => Some(tracing_root_filter()),
        _ => None,
    };

    let reporter_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_writer(reporter.clone())
        .without_time()
        .with_filter(tracing_output_filter());
    tracing_subscriber::registry()
        .with(root_filter)
        .with(tracing_console_layer)
        .with(reporter_layer)
        .with(log_file_layer)
        .with(opentelemetry_layer)
        .init();

    Ok((reporter, guard))
}

enum ConsoleReporter {
    SuperConsole {
        console: superconsole::SuperConsole,
        root: JobsComponent,
    },
    Plain {
        jobs: HashMap<JobId, Job>,
        job_outputs: Option<OutputBuffer<JobOutputStream>>,
    },
}

#[expect(clippy::print_stderr)]
impl ConsoleReporter {
    fn emit(&mut self, lines: superconsole::Lines) {
        match self {
            ConsoleReporter::SuperConsole { console, .. } => {
                console.emit(lines);
            }
            ConsoleReporter::Plain { .. } => {
                for line in lines {
                    eprintln!("{}", line.to_unstyled());
                }
            }
        }
    }

    fn add_job(&mut self, id: JobId, job: NewJob) {
        match self {
            ConsoleReporter::SuperConsole { root, .. } => {
                let new_job = Job::new(job);
                root.jobs.insert(id, new_job);
            }
            ConsoleReporter::Plain { jobs, .. } => {
                match &job {
                    NewJob::Download { url, started_at: _ } => {
                        eprintln!("Downloading {}", url);
                    }
                    NewJob::Unarchive { started_at: _ } => {}
                    NewJob::Process { status: _ } => {}
                    NewJob::CacheFetch {
                        kind: super::job::CacheFetchKind::Bake,
                        downloaded_data: _,
                        total_data: _,
                        downloaded_blobs: _,
                        total_blobs: _,
                        started_at: _,
                    } => {
                        eprintln!("Fetching artifact from cache");
                    }
                    NewJob::CacheFetch {
                        kind: super::job::CacheFetchKind::Project,
                        downloaded_data: _,
                        total_data: _,
                        downloaded_blobs: _,
                        total_blobs: _,
                        started_at: _,
                    } => {
                        eprintln!("Fetching project from cache");
                    }
                }

                let new_job = Job::new(job);
                jobs.insert(id, new_job);
            }
        }
    }

    fn update_job(&mut self, id: JobId, update: UpdateJob) {
        match self {
            ConsoleReporter::SuperConsole { root, .. } => {
                if let UpdateJob::ProcessPushPacket { ref packet, .. } = update {
                    let (stream, bytes) = match &packet.0 {
                        super::job::ProcessPacket::Stdout(bytes) => (ProcessStream::Stdout, bytes),
                        super::job::ProcessPacket::Stderr(bytes) => (ProcessStream::Stderr, bytes),
                    };
                    root.job_outputs
                        .append(JobOutputStream { job_id: id, stream }, bytes);
                } else if let UpdateJob::ProcessFlushPackets = update {
                    root.job_outputs.flush_stream(JobOutputStream {
                        job_id: id,
                        stream: ProcessStream::Stdout,
                    });
                    root.job_outputs.flush_stream(JobOutputStream {
                        job_id: id,
                        stream: ProcessStream::Stderr,
                    });
                };

                let Some(job) = root.jobs.get_mut(&id) else {
                    return;
                };
                let _ = job.update(update);
            }
            ConsoleReporter::Plain { jobs, job_outputs } => {
                let Some(job) = jobs.get(&id) else {
                    return;
                };

                match &update {
                    UpdateJob::Download { finished_at, .. } => {
                        if let Some(finished_at) = finished_at {
                            let elapsed = finished_at.saturating_duration_since(job.created_at());
                            eprintln!("Finished download in {}", DisplayDuration(elapsed));
                        }
                    }
                    UpdateJob::Unarchive { finished_at, .. } => {
                        if let Some(finished_at) = finished_at {
                            let elapsed = finished_at.saturating_duration_since(job.created_at());
                            eprintln!("Finished unarchiving in {}", DisplayDuration(elapsed));
                        }
                    }
                    UpdateJob::ProcessUpdateStatus { status } => {
                        let child_id = status.child_id();

                        let child_id_label = lazy_format::lazy_format! {
                            match (child_id) {
                                Some(child_id) => "{child_id}",
                                None => "?"
                            }
                        };

                        match status {
                            ProcessStatus::Preparing { .. } => {}
                            ProcessStatus::Running {
                                created_at,
                                started_at,
                                ..
                            } => {
                                let launch_duration =
                                    started_at.saturating_duration_since(*created_at);
                                if launch_duration > std::time::Duration::from_secs(1) {
                                    eprintln!(
                                        "Prepared process {child_id_label} in {}",
                                        DisplayDuration(launch_duration)
                                    );
                                }

                                eprintln!("Launched process {child_id_label}");
                            }
                            ProcessStatus::Ran {
                                started_at,
                                finished_at,
                                ..
                            } => {
                                let run_duration =
                                    finished_at.saturating_duration_since(*started_at);
                                eprintln!(
                                    "Process {child_id_label} ran in {}",
                                    DisplayDuration(run_duration)
                                );
                            }
                            ProcessStatus::Finalized {
                                finished_at,
                                finalized_at,
                                ..
                            } => {
                                let finalize_duration =
                                    finalized_at.saturating_duration_since(*finished_at);
                                if finalize_duration > std::time::Duration::from_secs(1) {
                                    eprintln!(
                                        "Process {child_id_label} finalized in {}",
                                        DisplayDuration(finalize_duration)
                                    );
                                }
                            }
                        }
                    }
                    UpdateJob::ProcessPushPacket { packet } => {
                        let (stream, bytes) = match &packet.0 {
                            super::job::ProcessPacket::Stdout(bytes) => {
                                (ProcessStream::Stdout, bytes)
                            }
                            super::job::ProcessPacket::Stderr(bytes) => {
                                (ProcessStream::Stderr, bytes)
                            }
                        };

                        if let Some(job_outputs) = job_outputs {
                            job_outputs.append(JobOutputStream { job_id: id, stream }, bytes);

                            while let Some((stream, content)) = job_outputs.pop_front() {
                                print_job_content(jobs, &stream, &content);
                            }
                        }
                    }
                    UpdateJob::ProcessFlushPackets => {
                        if let Some(job_outputs) = job_outputs {
                            job_outputs.flush_stream(JobOutputStream {
                                job_id: id,
                                stream: ProcessStream::Stdout,
                            });
                            job_outputs.flush_stream(JobOutputStream {
                                job_id: id,
                                stream: ProcessStream::Stderr,
                            });

                            while let Some((stream, content)) = job_outputs.pop_front() {
                                print_job_content(jobs, &stream, &content);
                            }
                        }
                    }
                    UpdateJob::CacheFetchAdd { .. } => {}
                    UpdateJob::CacheFetchUpdate { .. } => {}
                    UpdateJob::CacheFetchFinish { finished_at } => {
                        let elapsed = finished_at.saturating_duration_since(job.created_at());

                        let Job::CacheFetch {
                            kind,
                            downloaded_blobs,
                            ..
                        } = job
                        else {
                            panic!("tried to update non-cache job {id:?} with a cache update");
                        };
                        let fetch_kind = match kind {
                            crate::reporter::job::CacheFetchKind::Bake => "artifact",
                            crate::reporter::job::CacheFetchKind::Project => "project",
                        };

                        eprintln!(
                            "Finished fetching {fetch_kind} with {downloaded_blobs} new blob{s} from cache in {}",
                            DisplayDuration(elapsed),
                            s = if *downloaded_blobs == 1 { "" } else { "s" }
                        );
                    }
                }

                // This should never fail, since we would've already
                // returned early if the job ID wasn't found
                let job = jobs.get_mut(&id).expect("job not found");
                let _ = job.update(update);
            }
        }
    }

    fn render(&mut self) -> anyhow::Result<()> {
        match self {
            ConsoleReporter::SuperConsole { console, root } => {
                console.render(root)?;
            }
            ConsoleReporter::Plain { .. } => {}
        }

        Ok(())
    }

    fn finalize(self) -> anyhow::Result<()> {
        match self {
            ConsoleReporter::SuperConsole { console, root } => {
                console.finalize(&root)?;
            }
            ConsoleReporter::Plain { .. } => {}
        }

        anyhow::Ok(())
    }
}

#[expect(clippy::print_stderr)]
fn print_job_content(jobs: &HashMap<JobId, Job>, stream: &JobOutputStream, content: &[u8]) {
    let content = bstr::BStr::new(content);

    let job = jobs.get(&stream.job_id);
    let child_id = match job {
        Some(Job::Process { status, .. }) => status.child_id(),
        _ => None,
    };
    let child_id_label = lazy_format::lazy_format! {
        match (child_id) {
            Some(child_id) => "{child_id}",
            None => "?"
        }
    };

    let content = match content.strip_suffix(b"\n") {
        Some(content) => content,
        None => content,
    };

    for line in content.lines() {
        let line = bstr::BStr::new(line);
        eprintln!("[{child_id_label}] {line}");
    }
}

const JOB_LABEL_WIDTH: usize = 7;

struct JobsComponent {
    start: std::time::Instant,
    jobs: HashMap<JobId, Job>,
    job_outputs: OutputBuffer<JobOutputStream>,
}

impl superconsole::Component for JobsComponent {
    fn draw_unchecked(
        &self,
        dimensions: superconsole::Dimensions,
        mode: superconsole::DrawMode,
    ) -> anyhow::Result<superconsole::Lines> {
        let max_visible_jobs: usize = 4;

        let mut job_list: Vec<_> = self.jobs.iter().collect();
        job_list.sort_by(cmp_job_entries);

        let job_partition_point = job_list.partition_point(|&(_, job)| !job.is_complete());
        let (incomplete_jobs, complete_jobs) = job_list.split_at(job_partition_point);

        let num_incomplete_jobs = incomplete_jobs.len();
        let num_complete_jobs = complete_jobs.len();

        // Ensure we show at least one complete job (if there are any)
        let min_complete_jobs = std::cmp::min(num_complete_jobs, 1);
        let max_incomplete_jobs = max_visible_jobs.saturating_sub(min_complete_jobs);

        let job_list = incomplete_jobs
            .iter()
            .take(max_incomplete_jobs)
            .chain(complete_jobs)
            .take(max_visible_jobs);

        let jobs_lines = job_list
            .map(|(job_id, job)| {
                JobComponent(**job_id, job).draw(
                    superconsole::Dimensions {
                        width: dimensions.width,
                        height: 1,
                    },
                    mode,
                )
            })
            .collect::<Result<Vec<superconsole::Lines>, _>>()?;

        let num_job_output_lines = dimensions
            .height
            .saturating_sub(jobs_lines.len())
            .saturating_sub(3);

        let job_output_content_width = dimensions
            .width
            .saturating_sub(JOB_LABEL_WIDTH)
            .saturating_sub(4);

        let contents_rev = Some(self.job_outputs.contents().rev())
            .filter(|_| job_output_content_width > 0)
            .into_iter()
            .flatten();
        let lines_with_streams_rev = contents_rev
            .flat_map(|(stream, content)| {
                let content = match content.strip_suffix(b"\n") {
                    Some(content) => content,
                    None => content,
                };
                let lines_rev = content
                    .lines()
                    .rev()
                    .flat_map(|line| line.chunks(job_output_content_width).rev());
                lines_rev.map(|line| (*stream, bstr::BStr::new(line)))
            })
            .take(num_job_output_lines)
            .collect::<Vec<_>>();

        let mut job_output_lines = vec![];
        let mut last_job_id = None;
        for (stream, line) in lines_with_streams_rev.iter().rev() {
            // Merge job labels for consecutive lines for the same job
            let gutter = if last_job_id == Some(stream.job_id) {
                format!("{:1$}│ ", "", JOB_LABEL_WIDTH)
            } else {
                let job = self.jobs.get(&stream.job_id);
                let child_id = match job {
                    Some(Job::Process { status, .. }) => status.child_id(),
                    _ => None,
                };
                let job_label = match child_id {
                    Some(child_id) => format!("{child_id}"),
                    None => "?".to_string(),
                };
                let job_label = string_with_width(&job_label, JOB_LABEL_WIDTH, "…");

                format!("{job_label:0$}│ ", JOB_LABEL_WIDTH)
            };

            // Pick a color based on the job ID
            let gutter_color = job_color(stream.job_id);

            let styled_line = superconsole::Line::from_iter([
                superconsole::Span::new_colored_lossy(&gutter, gutter_color),
                superconsole::Span::new_unstyled_lossy(line),
            ]);
            job_output_lines.push(styled_line);

            last_job_id = Some(stream.job_id)
        }

        let summary_line = match mode {
            superconsole::DrawMode::Normal => {
                let elapsed_span = superconsole::Span::new_unstyled_lossy(
                    lazy_format::lazy_format!("{:>6}", DisplayDuration(self.start.elapsed())),
                );
                let running_jobs_span = superconsole::Span::new_colored_lossy(
                    &format!("{num_incomplete_jobs:3} running"),
                    if num_incomplete_jobs > 0 {
                        superconsole::style::Color::Blue
                    } else {
                        superconsole::style::Color::Grey
                    },
                );
                let complete_jobs_span = superconsole::Span::new_colored_lossy(
                    &format!("{num_complete_jobs:3} complete"),
                    if num_complete_jobs > 0 {
                        superconsole::style::Color::Green
                    } else {
                        superconsole::style::Color::Grey
                    },
                );
                let line = superconsole::Line::from_iter([
                    elapsed_span,
                    superconsole::Span::new_unstyled_lossy(" "),
                    complete_jobs_span,
                    superconsole::Span::new_unstyled_lossy(" "),
                    running_jobs_span,
                ]);
                Some(line)
            }
            superconsole::DrawMode::Final => {
                // Don't show the summary line on the final draw. The final
                // summary will be written outside the reporter, since we also
                // want to show the summary when not using SuperConsole
                None
            }
        };

        let lines = job_output_lines
            .into_iter()
            .chain(jobs_lines.into_iter().flatten())
            .chain(summary_line)
            .collect();
        Ok(lines)
    }
}

struct JobComponent<'a>(JobId, &'a Job);

impl superconsole::Component for JobComponent<'_> {
    fn draw_unchecked(
        &self,
        dimensions: superconsole::Dimensions,
        _mode: superconsole::DrawMode,
    ) -> anyhow::Result<superconsole::Lines> {
        let &JobComponent(job_id, job) = self;

        let elapsed_span = match (job.started_at(), job.finished_at()) {
            (Some(started_at), Some(finished_at)) => {
                let elapsed = finished_at.saturating_duration_since(started_at);
                let elapsed = DisplayDuration(elapsed);
                superconsole::Span::new_colored_lossy(
                    &format!("{elapsed:>6.8}"),
                    superconsole::style::Color::DarkGrey,
                )
            }
            (Some(started_at), None) => {
                let elapsed = started_at.elapsed();
                let elapsed = DisplayDuration(elapsed);
                superconsole::Span::new_colored_lossy(
                    &format!("{elapsed:>6.8}"),
                    superconsole::style::Color::Grey,
                )
            }
            (None, _) => {
                superconsole::Span::new_unstyled_lossy(lazy_format::lazy_format!("{:>6}", ""))
            }
        };

        let lines = match job {
            Job::Download {
                url,
                progress_percent,
                started_at: _,
                finished_at: _,
            } => {
                let percentage_span = match progress_percent {
                    Some(percent) => superconsole::Span::new_unstyled_lossy(
                        lazy_format::lazy_format!("{percent:>3}%"),
                    ),
                    None => superconsole::Span::new_unstyled_lossy("???%"),
                };

                let indicator = if job.is_complete() {
                    IndicatorKind::Complete
                } else {
                    IndicatorKind::Spinner(job.elapsed().unwrap_or_default())
                };

                let mut line = superconsole::Line::from_iter([
                    elapsed_span,
                    superconsole::Span::new_unstyled_lossy(" "),
                    indicator_span(indicator),
                    superconsole::Span::new_unstyled_lossy(" Download  "),
                    percentage_span,
                    superconsole::Span::new_unstyled_lossy(" "),
                ]);

                let remaining_width = dimensions
                    .width
                    .saturating_sub(1)
                    .saturating_sub(line.len());

                let truncated_url = string_with_width(url.as_str(), remaining_width, "…");

                let progress_bar_progress = progress_percent.unwrap_or(0) as f64 / 100.0;

                line.extend(progress_bar_spans(
                    &truncated_url,
                    remaining_width,
                    progress_bar_progress,
                ));

                superconsole::Lines::from_iter([line])
            }
            Job::Unarchive {
                progress_percent,
                started_at: _,
                finished_at: _,
            } => {
                let percentage_span = superconsole::Span::new_unstyled_lossy(
                    lazy_format::lazy_format!("{progress_percent:>3}%"),
                );

                let indicator = if job.is_complete() {
                    IndicatorKind::Complete
                } else {
                    IndicatorKind::Spinner(job.elapsed().unwrap_or_default())
                };

                let mut line = superconsole::Line::from_iter([
                    elapsed_span,
                    superconsole::Span::new_unstyled_lossy(" "),
                    indicator_span(indicator),
                    superconsole::Span::new_unstyled_lossy(" Unarchive "),
                    percentage_span,
                ]);

                if !job.is_complete() {
                    let remaining_width = dimensions
                        .width
                        .saturating_sub(1)
                        .saturating_sub(line.len());

                    let progress_bar_progress = *progress_percent as f64 / 100.0;

                    line.push(superconsole::Span::new_unstyled_lossy(" "));
                    line.extend(progress_bar_spans(
                        "",
                        remaining_width,
                        progress_bar_progress,
                    ));
                }

                superconsole::Lines::from_iter([line])
            }
            Job::Process {
                packet_queue: _,
                status,
            } => {
                let child_id = status
                    .child_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "?".to_string());

                let indicator = match status {
                    ProcessStatus::Preparing { created_at } => {
                        IndicatorKind::PreparingSpinner(created_at.elapsed())
                    }
                    ProcessStatus::Running { started_at, .. } => {
                        IndicatorKind::Spinner(started_at.elapsed())
                    }
                    ProcessStatus::Ran { .. } | ProcessStatus::Finalized { .. } => {
                        IndicatorKind::Complete
                    }
                };

                let note_span = match status {
                    ProcessStatus::Preparing { created_at } => {
                        let preparing_duration = created_at.elapsed();
                        if preparing_duration > std::time::Duration::from_secs(1) {
                            Some(superconsole::Span::new_colored_lossy(
                                &format!(
                                    " (preparing for {})",
                                    DisplayDuration(preparing_duration)
                                ),
                                superconsole::style::Color::DarkGrey,
                            ))
                        } else {
                            None
                        }
                    }
                    ProcessStatus::Ran { finished_at, .. } => {
                        let finalizing_duration = finished_at.elapsed();
                        if finalizing_duration > std::time::Duration::from_secs(1) {
                            Some(superconsole::Span::new_colored_lossy(
                                &format!(
                                    " (finalizing for {})",
                                    DisplayDuration(finalizing_duration)
                                ),
                                superconsole::style::Color::DarkGrey,
                            ))
                        } else {
                            None
                        }
                    }
                    ProcessStatus::Running { .. } | ProcessStatus::Finalized { .. } => None,
                };

                let child_id_span =
                    superconsole::Span::new_colored_lossy(&child_id, job_color(job_id));

                superconsole::Lines::from_iter([superconsole::Line::from_iter(
                    [
                        elapsed_span,
                        superconsole::Span::new_unstyled_lossy(" "),
                        indicator_span(indicator),
                        superconsole::Span::new_unstyled_lossy(" Process "),
                        child_id_span,
                    ]
                    .into_iter()
                    .chain(note_span),
                )])
            }
            Job::CacheFetch {
                kind,
                downloaded_data,
                total_data,
                downloaded_blobs,
                total_blobs,
                started_at: _,
                finished_at: _,
            } => {
                let total_progress = match total_data {
                    None => 0.0,
                    Some(0) => 1.0,
                    Some(total_data) => *downloaded_data as f64 / *total_data as f64,
                };
                let total_percent = (total_progress * 100.0) as u64;

                let percentage_span = superconsole::Span::new_unstyled_lossy(
                    lazy_format::lazy_format!("{total_percent:>3}%"),
                );

                let indicator = if job.is_complete() {
                    IndicatorKind::Complete
                } else {
                    IndicatorKind::Spinner(job.elapsed().unwrap_or_default())
                };

                let mut line = superconsole::Line::from_iter([
                    elapsed_span,
                    superconsole::Span::new_unstyled_lossy(" "),
                    indicator_span(indicator),
                    superconsole::Span::new_unstyled_lossy(" Cache     "),
                    percentage_span,
                    superconsole::Span::new_unstyled_lossy(" "),
                ]);

                let fetch_kind = match kind {
                    super::job::CacheFetchKind::Bake => "artifact",
                    super::job::CacheFetchKind::Project => "project",
                };
                let fetching_message = if job.is_complete() {
                    format!(
                        "Fetch {fetch_kind}: {downloaded_blobs} blob{s}",
                        s = if *downloaded_blobs != 1 { "s" } else { "" }
                    )
                } else if let Some(total_blobs) = total_blobs {
                    format!(
                        "Fetch {fetch_kind}: {downloaded_blobs} / {total_blobs} blob{s}",
                        s = if *downloaded_blobs != 1 { "s" } else { "" }
                    )
                } else {
                    format!("Fetch {fetch_kind}")
                };

                let remaining_width = dimensions
                    .width
                    .saturating_sub(1)
                    .saturating_sub(line.len());
                line.extend(progress_bar_spans(
                    &fetching_message,
                    remaining_width,
                    total_progress,
                ));

                superconsole::Lines::from_iter([line])
            }
        };

        Ok(lines)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct JobOutputStream {
    pub job_id: JobId,
    pub stream: ProcessStream,
}

fn cmp_job_entries(
    (a_id, a_job): &(&JobId, &Job),
    (b_id, b_job): &(&JobId, &Job),
) -> std::cmp::Ordering {
    let a_finalized_at = a_job.finalized_at();
    let b_finalized_at = b_job.finalized_at();

    // Show unfinalized jobs first
    match (a_finalized_at, b_finalized_at) {
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (a_finalized_at, b_finalized_at) => {
            // Show higher priority jobs first
            a_job
                .job_type_priority()
                .cmp(&b_job.job_type_priority())
                .reverse()
                .then_with(|| {
                    // Show more recently finalized jobs first
                    a_finalized_at.cmp(&b_finalized_at).reverse().then_with(|| {
                        // Show newer jobs first
                        a_id.cmp(b_id)
                    })
                })
        }
    }
}

fn string_with_width<'a>(s: &'a str, num_chars: usize, replacement: &str) -> Cow<'a, str> {
    if num_chars == 0 {
        return Cow::Borrowed("");
    }

    let s_chars = s.chars().count();

    match s_chars.cmp(&num_chars) {
        std::cmp::Ordering::Equal => Cow::Borrowed(s),
        std::cmp::Ordering::Less => Cow::Owned(format!("{s:0$}", num_chars)),
        std::cmp::Ordering::Greater => {
            let replacement_chars = replacement.chars().count();
            let keep_chars = num_chars.saturating_sub(replacement_chars);

            let left_chars = keep_chars / 2;
            let right_chars = keep_chars.saturating_sub(left_chars);

            let new_chars = s
                .chars()
                .take(left_chars)
                .chain(replacement.chars().take(num_chars))
                .chain(s.chars().skip(s_chars.saturating_sub(right_chars)));

            Cow::Owned(new_chars.collect())
        }
    }
}

fn job_color(job_id: JobId) -> superconsole::style::Color {
    const JOB_COLORS: &[superconsole::style::Color] = &[
        superconsole::style::Color::Cyan,
        superconsole::style::Color::Magenta,
        superconsole::style::Color::Yellow,
        superconsole::style::Color::Blue,
    ];

    JOB_COLORS[job_id.0 % JOB_COLORS.len()]
}

#[derive(Debug, Clone, Copy)]
enum IndicatorKind {
    PreparingSpinner(std::time::Duration),
    Spinner(std::time::Duration),
    Complete,
}

fn indicator_span(kind: IndicatorKind) -> superconsole::Span {
    match kind {
        IndicatorKind::Spinner(elapsed) => {
            let spinner = spinner(elapsed, 100);
            superconsole::Span::new_colored_lossy(spinner, superconsole::style::Color::Blue)
        }
        IndicatorKind::PreparingSpinner(elapsed) => {
            let spinner = spinner(elapsed, 200);
            superconsole::Span::new_colored_lossy(spinner, superconsole::style::Color::Grey)
        }
        IndicatorKind::Complete => {
            superconsole::Span::new_colored_lossy("✓", superconsole::style::Color::Green)
        }
    }
}

fn progress_bar_spans(interior: &str, width: usize, progress: f64) -> [superconsole::Span; 2] {
    let filled = ((width as f64) * progress) as usize;

    let padded = format!("{interior:0$.0$}", width);
    let (filled_part, unfilled_part) = if progress <= 0.0 {
        ("", &*padded)
    } else if progress >= 1.0 {
        (&*padded, "")
    } else {
        let split_offset = padded
            .char_indices()
            .find_map(
                |(index, _)| {
                    if index >= filled {
                        Some(index)
                    } else {
                        None
                    }
                },
            )
            .unwrap_or(0);
        padded.split_at(split_offset)
    };

    [
        superconsole::Span::new_styled_lossy(filled_part.to_string().dark_grey().negative()),
        superconsole::Span::new_colored_lossy(unfilled_part, superconsole::style::Color::DarkGrey),
    ]
}

fn spinner(duration: std::time::Duration, speed: u128) -> &'static str {
    const SPINNERS: &[&str] = &["◜", "◝", "◞", "◟"];
    SPINNERS[(duration.as_millis() / speed) as usize % SPINNERS.len()]
}

#[cfg(test)]
mod tests {
    use super::string_with_width;

    #[test]
    fn test_string_with_width() {
        assert_eq!(string_with_width("abcd", 10, "-"), "abcd      ");
        assert_eq!(string_with_width("abcd", 4, "-"), "abcd");
        assert_eq!(string_with_width("abcd", 3, "-"), "a-d");
        assert_eq!(string_with_width("abcd", 1, "-"), "-");
        assert_eq!(string_with_width("abcd", 0, "-"), "");

        assert_eq!(string_with_width("abcde", 10, "-"), "abcde     ");
        assert_eq!(string_with_width("abcde", 5, "-"), "abcde");
        assert_eq!(string_with_width("abcde", 3, "-"), "a-e");
        assert_eq!(string_with_width("abcde", 1, "-"), "-");
        assert_eq!(string_with_width("abcde", 0, "-"), "");

        assert_eq!(string_with_width("abcd", 10, "…"), "abcd      ");
        assert_eq!(string_with_width("abcd", 4, "…"), "abcd");
        assert_eq!(string_with_width("abcd", 3, "…"), "a…d");
        assert_eq!(string_with_width("abcd", 1, "…"), "…");
        assert_eq!(string_with_width("abcd", 0, "…"), "");

        assert_eq!(string_with_width("abcde", 10, "…"), "abcde     ");
        assert_eq!(string_with_width("abcde", 5, "…"), "abcde");
        assert_eq!(string_with_width("abcde", 3, "…"), "a…e");
        assert_eq!(string_with_width("abcde", 1, "…"), "…");
        assert_eq!(string_with_width("abcde", 0, "…"), "");

        assert_eq!(string_with_width("abcdef", 10, "..."), "abcdef    ");
        assert_eq!(string_with_width("abcdef", 6, "..."), "abcdef");
        assert_eq!(string_with_width("abcdef", 5, "..."), "a...f");
        assert_eq!(string_with_width("abcdef", 3, "..."), "...");
        assert_eq!(string_with_width("abcdef", 1, "..."), ".");
        assert_eq!(string_with_width("abcdef", 0, "..."), "");

        assert_eq!(string_with_width("abcdefg", 10, "..."), "abcdefg   ");
        assert_eq!(string_with_width("abcdefg", 7, "..."), "abcdefg");
        assert_eq!(string_with_width("abcdefg", 5, "..."), "a...g");
        assert_eq!(string_with_width("abcdefg", 3, "..."), "...");
        assert_eq!(string_with_width("abcdefg", 1, "..."), ".");
        assert_eq!(string_with_width("abcdefg", 0, "..."), "");
    }
}
