use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc,
    },
};

use bstr::ByteSlice as _;
use human_repr::HumanDuration as _;
use opentelemetry::trace::TracerProvider as _;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer as _};

use super::{
    job::{Job, NewJob, ProcessStatus, UpdateJob},
    tracing_debug_filter, tracing_output_filter, tracing_root_filter, JobId, ReportEvent, Reporter,
    ReporterGuard,
};

#[derive(Debug, Clone, Copy)]
pub enum ConsoleReporterKind {
    Auto,
    SuperConsole,
    Plain,
}

pub fn start_console_reporter(
    kind: ConsoleReporterKind,
) -> anyhow::Result<(Reporter, ReporterGuard)> {
    let jobs = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
    let queued_lines = Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let brioche_otel_enabled = matches!(
        std::env::var("BRIOCHE_ENABLE_OTEL").as_deref(),
        Ok("1") | Ok("true")
    );

    let start = std::time::Instant::now();
    let is_evaluating = Arc::new(AtomicBool::new(false));

    let reporter = Reporter {
        start,
        num_jobs: Arc::new(AtomicUsize::new(0)),
        is_evaluating: is_evaluating.clone(),
        tx: tx.clone(),
    };
    let guard = ReporterGuard {
        tx,
        shutdown_rx: Some(shutdown_rx),
        shutdown_opentelemetry: brioche_otel_enabled,
    };

    std::thread::spawn({
        let queued_lines = queued_lines.clone();
        let jobs = jobs.clone();
        move || {
            let superconsole = match kind {
                ConsoleReporterKind::Auto => superconsole::SuperConsole::new(),
                ConsoleReporterKind::SuperConsole => Some(superconsole::SuperConsole::forced_new(
                    superconsole::Dimensions {
                        width: 80,
                        height: 24,
                    },
                )),
                ConsoleReporterKind::Plain => None,
            };
            let mut console = match superconsole {
                Some(console) => {
                    let root = JobsComponent {
                        start,
                        is_evaluating,
                        jobs,
                        terminal: tokio::sync::RwLock::new(termwiz::surface::Surface::new(80, 24)),
                    };
                    ConsoleReporter::SuperConsole {
                        console,
                        root,
                        partial_lines: HashMap::new(),
                    }
                }
                None => ConsoleReporter::Plain {
                    partial_lines: HashMap::new(),
                },
            };

            let mut running = true;
            while running {
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

                let _ = console.render();

                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            let _ = console.finalize();
            let _ = shutdown_tx.send(());
        }
    });

    let opentelemetry_layer = if brioche_otel_enabled {
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let provider = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .http()
                    .with_http_client(reqwest::Client::new()),
            )
            .with_trace_config(opentelemetry_sdk::trace::Config::default().with_resource(
                opentelemetry_sdk::Resource::default().merge(&opentelemetry_sdk::Resource::new(
                    vec![
                        opentelemetry::KeyValue::new(
                            opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                            "brioche",
                        ),
                        opentelemetry::KeyValue::new(
                            opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
                            env!("CARGO_PKG_VERSION"),
                        ),
                    ],
                )),
            ))
            .install_simple()?;

        Some(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer("tracing-opentelemetry"))
                .with_filter(tracing_debug_filter()),
        )
    } else {
        None
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
        partial_lines: HashMap<JobId, Vec<u8>>,
    },
    Plain {
        partial_lines: HashMap<JobId, Vec<u8>>,
    },
}

impl ConsoleReporter {
    fn emit(&mut self, lines: superconsole::Lines) {
        match self {
            ConsoleReporter::SuperConsole { console, .. } => {
                console.emit(lines);
            }
            ConsoleReporter::Plain { partial_lines: _ } => {
                for line in lines {
                    eprintln!("{}", line.to_unstyled());
                }
            }
        }
    }

    fn add_job(&mut self, id: JobId, job: NewJob) {
        match self {
            ConsoleReporter::SuperConsole { root, .. } => {
                let mut jobs = root.jobs.blocking_write();
                let new_job = Job::new(job);
                jobs.insert(id, new_job);
            }
            ConsoleReporter::Plain { partial_lines: _ } => match job {
                NewJob::Download { url } => {
                    eprintln!("Downloading {}", url);
                }
                NewJob::Unarchive => {}
                NewJob::Process { status } => {
                    if let Some(child_id) = status.child_id() {
                        eprintln!("Started process {child_id}");
                    } else {
                        eprintln!("Started process");
                    }
                }
                NewJob::RegistryFetch {
                    total_blobs,
                    total_recipes,
                } => {
                    eprintln!(
                        "Fetching {total_blobs} blob{} / {total_recipes} recipe{} from registry",
                        if total_blobs == 1 { "" } else { "s" },
                        if total_recipes == 1 { "" } else { "s" },
                    );
                }
            },
        }
    }

    fn update_job(&mut self, id: JobId, update: UpdateJob) {
        match self {
            ConsoleReporter::SuperConsole {
                root,
                partial_lines,
                ..
            } => {
                if let UpdateJob::Process {
                    ref packet,
                    ref status,
                } = update
                {
                    let mut terminal = root.terminal.blocking_write();
                    if let Some(packet) = &packet.0 {
                        let child_id = status
                            .child_id()
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        let buffer = partial_lines.entry(id).or_default();
                        buffer.extend_from_slice(packet.bytes());

                        if let Some((lines, remainder)) = buffer.rsplit_once_str(b"\n") {
                            // Write each output line to the terminal, preceded
                            // by the process ID. We also use "\r\n" since we're
                            // writing to a terminal-like output.
                            for line in lines.split(|&b| b == b'\n') {
                                terminal.add_change("\r\n");
                                terminal.add_change(format!("[{child_id}] "));
                                terminal.add_change(String::from_utf8_lossy(line));
                            }

                            *buffer = remainder.to_vec();
                        }
                    }
                };

                let mut jobs = root.jobs.blocking_write();
                let Some(job) = jobs.get_mut(&id) else {
                    return;
                };
                let _ = job.update(update);
            }
            ConsoleReporter::Plain { partial_lines } => match update {
                UpdateJob::Download { progress_percent } => {
                    if progress_percent == Some(100) {
                        eprintln!("Finished download");
                    }
                }
                UpdateJob::Unarchive { progress_percent } => {
                    if progress_percent == 100 {
                        eprintln!("Unarchive");
                    }
                }
                UpdateJob::Process { mut packet, status } => {
                    let child_id = status
                        .child_id()
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "?".to_string());

                    if let Some(packet) = packet.take() {
                        let buffer = partial_lines.entry(id).or_default();
                        buffer.extend_from_slice(packet.bytes());
                        if let Some((lines, remainder)) = buffer.rsplit_once_str(b"\n") {
                            let lines = bstr::BStr::new(lines);
                            for line in lines.lines() {
                                eprintln!("[{child_id}] {}", bstr::BStr::new(line));
                            }
                            *buffer = remainder.to_vec();
                        }
                    }

                    match status {
                        ProcessStatus::Running { .. } => {}
                        ProcessStatus::Exited { status, .. } => {
                            if let Some(code) = status.as_ref().and_then(|status| status.code()) {
                                eprintln!("Process {child_id} exited with code {}", code);
                            } else {
                                eprintln!("Process {child_id} exited");
                            }
                        }
                    }
                }
                UpdateJob::RegistryFetchAdd { .. } => {}
                UpdateJob::RegistryFetchUpdate { .. } => {}
                UpdateJob::RegistryFetchFinish => {
                    eprintln!("Finished fetching from registry");
                }
            },
        }
    }

    fn render(&mut self) -> anyhow::Result<()> {
        match self {
            ConsoleReporter::SuperConsole {
                console,
                root,
                partial_lines: _,
            } => {
                console.render(root)?;
            }
            ConsoleReporter::Plain { .. } => {}
        }

        Ok(())
    }

    fn finalize(self) -> anyhow::Result<()> {
        match self {
            ConsoleReporter::SuperConsole {
                console,
                root,
                partial_lines: _,
            } => {
                console.finalize(&root)?;
            }
            ConsoleReporter::Plain { .. } => {}
        }

        anyhow::Ok(())
    }
}

struct JobsComponent {
    start: std::time::Instant,
    is_evaluating: Arc<AtomicBool>,
    jobs: Arc<tokio::sync::RwLock<HashMap<JobId, Job>>>,
    terminal: tokio::sync::RwLock<termwiz::surface::Surface>,
}

impl superconsole::Component for JobsComponent {
    fn draw_unchecked(
        &self,
        dimensions: superconsole::Dimensions,
        mode: superconsole::DrawMode,
    ) -> anyhow::Result<superconsole::Lines> {
        let jobs = self.jobs.blocking_read();
        let mut jobs: Vec<_> = jobs.iter().collect();
        let max_visible_jobs = std::cmp::max(dimensions.height.saturating_sub(15), 3);

        jobs.sort_by(cmp_job_entries);
        let job_partition_point = jobs.partition_point(|&(_, job)| !job.is_complete());
        let (incomplete_jobs, complete_jobs) = jobs.split_at(job_partition_point);

        let num_jobs = jobs.len();
        let num_complete_jobs = complete_jobs.len();
        let is_evaluating = self.is_evaluating.load(std::sync::atomic::Ordering::SeqCst);

        let jobs = incomplete_jobs
            .iter()
            .chain(complete_jobs.iter().take(3))
            .take(max_visible_jobs);

        let jobs_lines = jobs
            .map(|(_, job)| {
                job.draw(
                    superconsole::Dimensions {
                        width: dimensions.width,
                        height: 1,
                    },
                    mode,
                )
            })
            .collect::<Result<Vec<superconsole::Lines>, _>>()?;

        let num_terminal_lines = dimensions
            .height
            .saturating_sub(jobs_lines.len())
            .saturating_sub(3);
        let mut terminal = self.terminal.blocking_write();

        terminal.resize(dimensions.width, std::cmp::max(num_terminal_lines, 1));

        let terminal_lines = terminal.screen_lines();
        let terminal_lines = terminal_lines
            .iter()
            .skip_while(|line| line.is_whitespace())
            .map(|line| superconsole::Line::sanitized(&line.as_str()))
            .take(num_terminal_lines);

        let elapsed = self.start.elapsed().human_duration();
        let summary_line = match mode {
            superconsole::DrawMode::Normal => {
                let summary_line = format!(
                    "[{elapsed}] {num_complete_jobs} / {num_jobs}{or_more} job{s} complete",
                    s = if num_jobs == 1 { "" } else { "s" },
                    or_more = if is_evaluating { "+" } else { "" },
                );
                Some(superconsole::Line::from_iter([summary_line
                    .try_into()
                    .unwrap()]))
            }
            superconsole::DrawMode::Final => {
                // Don't show the summary line on the final draw. The final
                // summary will be written outside the reporter, since we also
                // want to show the summary when not using SuperConsole
                None
            }
        };

        let lines = terminal_lines
            .chain(jobs_lines.into_iter().flatten())
            .chain(summary_line)
            .collect();
        Ok(lines)
    }
}

fn cmp_job_entries(
    (a_id, a_job): &(&JobId, &Job),
    (b_id, b_job): &(&JobId, &Job),
) -> std::cmp::Ordering {
    let a_is_complete = a_job.is_complete();
    let b_is_complete = b_job.is_complete();

    // Show incomplete jobs first
    a_is_complete.cmp(&b_is_complete).then_with(|| {
        if a_is_complete {
            // If both jobs are complete, then show the highest priority jobs
            // first, then show the newest job first
            a_job
                .job_type_priority()
                .cmp(&b_job.job_type_priority())
                .reverse()
                .then_with(|| a_id.cmp(b_id).reverse())
        } else {
            // If neither jobs is complete, then show the oldest job first
            a_id.cmp(b_id)
        }
    })
}
