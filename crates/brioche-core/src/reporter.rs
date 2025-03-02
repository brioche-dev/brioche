use std::sync::{Arc, atomic::AtomicUsize};

use tracing::Instrument as _;
use tracing_subscriber::{Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _};

pub mod console;
pub mod job;

const DEFAULT_TRACING_LEVEL: &str = "brioche=info";
const DEFAULT_DEBUG_TRACING_LEVEL: &str = "brioche=debug";

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

pub fn start_lsp_reporter(client: tower_lsp::Client) -> (Reporter, ReporterGuard) {
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();

    let reporter = Reporter {
        start: std::time::Instant::now(),
        num_jobs: Arc::new(AtomicUsize::new(0)),
        tx: tx.clone(),
    };
    let guard = ReporterGuard {
        tx,
        shutdown_rx: None,
        opentelemetry_tracer_provider: None,
    };

    let (lsp_tx, mut lsp_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(
        async move {
            while let Some((message_type, message)) = lsp_rx.recv().await {
                let _ = client.log_message(message_type, message).await;
            }
        }
        .instrument(tracing::Span::current()),
    );

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

pub fn start_null_reporter() -> (Reporter, ReporterGuard) {
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();

    let reporter = Reporter {
        start: std::time::Instant::now(),
        num_jobs: Arc::new(AtomicUsize::new(0)),
        tx: tx.clone(),
    };
    let guard = ReporterGuard {
        tx,
        shutdown_rx: None,
        opentelemetry_tracer_provider: None,
    };

    (reporter, guard)
}

pub fn start_test_reporter() -> (Reporter, ReporterGuard) {
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();

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
        start: std::time::Instant::now(),
        num_jobs: Arc::new(AtomicUsize::new(0)),
        tx: tx.clone(),
    };
    let guard = ReporterGuard {
        tx,
        shutdown_rx: None,
        opentelemetry_tracer_provider: None,
    };

    (reporter, guard)
}

pub struct ReporterGuard {
    tx: tokio::sync::mpsc::UnboundedSender<ReportEvent>,
    shutdown_rx: Option<tokio::sync::oneshot::Receiver<()>>,
    opentelemetry_tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
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

        if let Some(provider) = &self.opentelemetry_tracer_provider {
            let _ = provider.shutdown();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(usize);

#[derive(Debug, Clone)]
pub struct Reporter {
    start: std::time::Instant,
    num_jobs: Arc<AtomicUsize>,
    tx: tokio::sync::mpsc::UnboundedSender<ReportEvent>,
}

impl Reporter {
    pub fn emit(&self, lines: superconsole::Lines) {
        let _ = self.tx.send(ReportEvent::Emit { lines });
    }

    pub fn add_job(&self, job: job::NewJob) -> JobId {
        let id = self
            .num_jobs
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id = JobId(id);

        let _ = self.tx.send(ReportEvent::AddJob { id, job });

        id
    }

    pub fn update_job(&self, id: JobId, update: job::UpdateJob) {
        let _ = self.tx.send(ReportEvent::UpdateJobState { id, update });
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
    }

    pub fn num_jobs(&self) -> usize {
        self.num_jobs.load(std::sync::atomic::Ordering::SeqCst)
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

enum ReportEvent {
    Emit { lines: superconsole::Lines },
    AddJob { id: JobId, job: job::NewJob },
    UpdateJobState { id: JobId, update: job::UpdateJob },
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
