use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc, RwLock,
    },
};

use bstr::ByteSlice;
use debug_ignore::DebugIgnore;
use human_repr::HumanDuration as _;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer as _};

const DEFAULT_TRACING_LEVEL: &str = "brioche=info";
const DEFAULT_DEBUG_TRACING_LEVEL: &str = "brioche=debug";

pub fn start_console_reporter() -> anyhow::Result<(Reporter, ReporterGuard)> {
    let jobs = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
    let queued_lines = Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let brioche_jaeger_endpoint = std::env::var("BRIOCHE_JAEGER_ENDPOINT").ok();

    let is_evaluating = Arc::new(AtomicBool::new(false));

    let reporter = Reporter {
        tx: tx.clone(),
        is_evaluating: is_evaluating.clone(),
        next_id: Arc::new(AtomicUsize::new(0)),
    };
    let guard = ReporterGuard {
        tx,
        shutdown_rx: Some(shutdown_rx),
        shutdown_opentelemetry: brioche_jaeger_endpoint.is_some(),
    };

    std::thread::spawn({
        let queued_lines = queued_lines.clone();
        let jobs = jobs.clone();
        move || {
            let superconsole = superconsole::SuperConsole::new();
            let mut console = match superconsole {
                Some(console) => {
                    let root = JobsComponent {
                        start: std::time::Instant::now(),
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

    let opentelemetry_layer = brioche_jaeger_endpoint
        .map(|jaeger_endpoint| {
            opentelemetry::global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
            let tracer = opentelemetry_jaeger::new_agent_pipeline()
                .with_service_name("brioche")
                .with_endpoint(jaeger_endpoint)
                .install_simple()?;

            anyhow::Ok(
                tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(tracing_debug_filter()),
            )
        })
        .transpose()?;

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

fn tracing_output_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::builder()
        .with_default_directive(DEFAULT_TRACING_LEVEL.parse().expect("invalid filter"))
        .from_env_lossy()
}

fn tracing_debug_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::builder()
        .with_default_directive(DEFAULT_DEBUG_TRACING_LEVEL.parse().expect("invalid filter"))
        .with_env_var("BRIOCHE_LOG_DEBUG")
        .from_env_lossy()
}

fn tracing_root_filter() -> tracing_subscriber::EnvFilter {
    tracing_debug_filter()
        .add_directive("tokio=trace".parse().expect("invalid filter"))
        .add_directive("runtime=trace".parse().expect("invalid filter"))
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
                NewJob::Unpack => {}
                NewJob::Process { status } => {
                    if let Some(child_id) = status.child_id() {
                        eprintln!("Started process {child_id}");
                    } else {
                        eprintln!("Started process");
                    }
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
                UpdateJob::Unpack { progress_percent } => {
                    if progress_percent == 100 {
                        eprintln!("Unpacked");
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

pub fn start_lsp_reporter(client: tower_lsp::Client) -> (Reporter, ReporterGuard) {
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();

    let reporter = Reporter {
        next_id: Arc::new(AtomicUsize::new(0)),
        is_evaluating: Arc::new(AtomicBool::new(false)),
        tx: tx.clone(),
    };
    let guard = ReporterGuard {
        tx,
        shutdown_rx: None,
        shutdown_opentelemetry: false,
    };

    let (lsp_tx, mut lsp_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(async move {
        while let Some((message_type, message)) = lsp_rx.recv().await {
            let _ = client.log_message(message_type, message).await;
        }
    });

    let lsp_client_writer = LspClientWriter { lsp_tx };

    let tracing_console_layer =
        std::env::var_os("BRIOCHE_CONSOLE").map(|_| console_subscriber::spawn());
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(lsp_client_writer)
        .with_ansi(false)
        .without_time()
        .with_filter(tracing_output_filter());

    let root_filter = if tracing_console_layer.is_some() {
        Some(tracing_root_filter())
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(root_filter)
        .with(tracing_console_layer)
        .with(fmt_layer)
        .init();

    (reporter, guard)
}

#[cfg_attr(not(test), allow(unused))]
pub fn start_test_reporter() -> (Reporter, ReporterGuard) {
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let (_, shutdown_rx) = tokio::sync::oneshot::channel();

    static TEST_TRACING_SUBSCRIBER: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if let Some(debug_output_path) = std::env::var_os("BRIOCHE_LOG_OUTPUT") {
        TEST_TRACING_SUBSCRIBER.get_or_init(|| {
            let debug_output = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(debug_output_path)
                .expect("failed to open debug output path");
            tracing_subscriber::fmt()
                .json()
                .with_writer(debug_output)
                .with_timer(tracing_subscriber::fmt::time::uptime())
                .with_env_filter(tracing_debug_filter())
                .init();
        });
    };

    let reporter = Reporter {
        next_id: Arc::new(AtomicUsize::new(0)),
        is_evaluating: Arc::new(AtomicBool::new(false)),
        tx: tx.clone(),
    };
    let guard = ReporterGuard {
        tx,
        shutdown_rx: Some(shutdown_rx),
        shutdown_opentelemetry: false,
    };

    (reporter, guard)
}

pub struct ReporterGuard {
    tx: tokio::sync::mpsc::UnboundedSender<ReportEvent>,
    shutdown_rx: Option<tokio::sync::oneshot::Receiver<()>>,
    shutdown_opentelemetry: bool,
}

impl ReporterGuard {
    pub async fn shutdown_console(&mut self) {
        let _ = self.tx.send(ReportEvent::Shutdown);
        if let Some(shutdown_rx) = self.shutdown_rx.take() {
            let _ = shutdown_rx.await;
        }
    }
}

impl Drop for ReporterGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(ReportEvent::Shutdown);
        if let Some(stop_rx) = self.shutdown_rx.take() {
            futures::executor::block_on(async {
                // Wait for stop_rx or wait 500ms
                tokio::select! {
                    _ = stop_rx => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
                }
            });
        }

        if self.shutdown_opentelemetry {
            opentelemetry::global::shutdown_tracer_provider();
        }
    }
}

#[derive(Debug)]
pub enum NewJob {
    Download { url: url::Url },
    Unpack,
    Process { status: ProcessStatus },
}

#[derive(Debug)]
pub enum UpdateJob {
    Download {
        progress_percent: Option<u8>,
    },
    Unpack {
        progress_percent: u8,
    },
    Process {
        packet: DebugIgnore<Option<ProcessPacket>>,
        status: ProcessStatus,
    },
}

#[derive(Debug)]
pub enum Job {
    Download {
        url: url::Url,
        progress_percent: Option<u8>,
    },
    Unpack {
        progress_percent: u8,
    },
    Process {
        packet_queue: DebugIgnore<Arc<RwLock<Vec<ProcessPacket>>>>,
        status: ProcessStatus,
    },
}

impl Job {
    fn new(new: NewJob) -> Self {
        match new {
            NewJob::Download { url } => Self::Download {
                url,
                progress_percent: Some(0),
            },
            NewJob::Unpack => Self::Unpack {
                progress_percent: 0,
            },
            NewJob::Process { status } => Self::Process {
                packet_queue: Default::default(),
                status,
            },
        }
    }

    fn update(&mut self, update: UpdateJob) -> anyhow::Result<()> {
        match update {
            UpdateJob::Download {
                progress_percent: new_progress_percent,
            } => {
                let Self::Download {
                    progress_percent, ..
                } = self
                else {
                    anyhow::bail!("tried to update a non-download job with a download update");
                };
                *progress_percent = new_progress_percent;
            }
            UpdateJob::Unpack {
                progress_percent: new_progress_percent,
            } => {
                let Self::Unpack {
                    progress_percent, ..
                } = self
                else {
                    anyhow::bail!("tried to update a non-unpack job with an unpack update");
                };
                *progress_percent = new_progress_percent;
            }
            UpdateJob::Process {
                mut packet,
                status: new_status,
            } => {
                let Self::Process {
                    packet_queue,
                    status,
                } = self
                else {
                    anyhow::bail!("tried to update a non-process job with a process update");
                };

                if let Some(packet) = packet.take() {
                    let mut packet_queue = packet_queue.write().map_err(|_| {
                        anyhow::anyhow!("failed to lock process packet queue for writing")
                    })?;
                    packet_queue.push(packet);
                }
                *status = new_status;
            }
        }

        Ok(())
    }

    fn is_complete(&self) -> bool {
        match self {
            Job::Download {
                progress_percent, ..
            } => progress_percent.map(|p| p >= 100).unwrap_or(false),
            Job::Unpack { progress_percent } => *progress_percent >= 100,
            Job::Process {
                status,
                packet_queue: _,
            } => matches!(status, ProcessStatus::Exited { .. }),
        }
    }

    // Returns a priority for the job type. 0 is the lowest priority. Higher
    // priority jobs are displayed first.
    fn job_type_priority(&self) -> u8 {
        match self {
            Job::Unpack { .. } => 0,
            Job::Download { .. } => 1,
            Job::Process { .. } => 2,
        }
    }
}

impl superconsole::Component for Job {
    fn draw_unchecked(
        &self,
        _dimensions: superconsole::Dimensions,
        _mode: superconsole::DrawMode,
    ) -> anyhow::Result<superconsole::Lines> {
        let lines = match self {
            Job::Download {
                url,
                progress_percent,
            } => {
                let message = match progress_percent {
                    Some(100) => {
                        format!("[100%] Downloaded {url}")
                    }
                    Some(progress_percent) => {
                        format!("[{progress_percent:>3}%] Downloading {url}")
                    }
                    None => {
                        format!("[???%] Downloading {url}")
                    }
                };
                superconsole::Lines::from_iter([superconsole::Line::sanitized(&message)])
            }
            Job::Unpack { progress_percent } => {
                let message = if *progress_percent == 100 {
                    "[100%] Unpacked".to_string()
                } else {
                    format!("[{progress_percent:>3}%] Unpacking")
                };
                superconsole::Lines::from_iter([superconsole::Line::sanitized(&message)])
            }
            Job::Process {
                packet_queue: _,
                status,
            } => {
                let child_id = status
                    .child_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let elapsed = status.elapsed().human_duration();
                let message = match status {
                    ProcessStatus::Running { .. } => {
                        format!("Process {child_id} [{elapsed}]")
                    }
                    ProcessStatus::Exited { status, .. } => {
                        let status = status
                            .as_ref()
                            .and_then(|status| status.code())
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        format!("Process {child_id} [{elapsed} Exited {status}]")
                    }
                };

                superconsole::Lines::from_iter(std::iter::once(superconsole::Line::sanitized(
                    &message,
                )))
            }
        };

        Ok(lines)
    }
}

pub enum ProcessPacket {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

impl ProcessPacket {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Stdout(bytes) => bytes,
            Self::Stderr(bytes) => bytes,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProcessStatus {
    Running {
        child_id: Option<u32>,
        start: std::time::Instant,
    },
    Exited {
        child_id: Option<u32>,
        status: Option<std::process::ExitStatus>,
        elapsed: std::time::Duration,
    },
}

impl ProcessStatus {
    fn child_id(&self) -> Option<u32> {
        match self {
            ProcessStatus::Running { child_id, .. } => *child_id,
            ProcessStatus::Exited { child_id, .. } => *child_id,
        }
    }

    fn elapsed(&self) -> std::time::Duration {
        match self {
            ProcessStatus::Running { start, .. } => start.elapsed(),
            ProcessStatus::Exited { elapsed, .. } => *elapsed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(usize);

#[derive(Debug, Clone)]
pub struct Reporter {
    tx: tokio::sync::mpsc::UnboundedSender<ReportEvent>,
    is_evaluating: Arc<AtomicBool>,
    next_id: Arc<AtomicUsize>,
}

impl Reporter {
    pub fn emit(&self, lines: superconsole::Lines) {
        let _ = self.tx.send(ReportEvent::Emit { lines });
    }

    pub fn set_is_evaluating(&self, is_evaluating: bool) {
        self.is_evaluating
            .store(is_evaluating, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn add_job(&self, job: NewJob) -> JobId {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id = JobId(id);

        let _ = self.tx.send(ReportEvent::AddJob { id, job });

        id
    }

    pub fn update_job(&self, id: JobId, update: UpdateJob) {
        let _ = self.tx.send(ReportEvent::UpdateJobState { id, update });
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for Reporter {
    type Writer = ReporterWriter;

    fn make_writer(&self) -> Self::Writer {
        ReporterWriter {
            reporter: self.clone(),
        }
    }
}

pub struct ReporterWriter {
    reporter: Reporter,
}

impl std::io::Write for ReporterWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.reporter
            .emit(superconsole::Lines::from_colored_multiline_string(
                &String::from_utf8_lossy(buf),
            ));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
        let summary_line = format!(
            "[{elapsed}] {num_complete_jobs} / {num_jobs}{or_more} job{s} complete",
            s = if num_jobs == 1 { "" } else { "s" },
            or_more = if is_evaluating { "+" } else { "" },
        );
        let summary_line = superconsole::Line::from_iter([summary_line.try_into().unwrap()]);

        let lines = terminal_lines
            .chain(jobs_lines.into_iter().flatten())
            .chain([summary_line])
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

enum ReportEvent {
    Emit { lines: superconsole::Lines },
    AddJob { id: JobId, job: NewJob },
    UpdateJobState { id: JobId, update: UpdateJob },
    Shutdown,
}

#[derive(Debug, Clone)]
struct LspClientWriter {
    lsp_tx: tokio::sync::mpsc::UnboundedSender<(tower_lsp::lsp_types::MessageType, String)>,
}

impl std::io::Write for LspClientWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.lsp_tx
            .send((
                tower_lsp::lsp_types::MessageType::INFO,
                String::from_utf8_lossy(buf).to_string(),
            ))
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for LspClientWriter {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}
