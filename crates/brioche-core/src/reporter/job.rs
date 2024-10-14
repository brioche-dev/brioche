use std::sync::{Arc, RwLock};

use debug_ignore::DebugIgnore;
use human_repr::HumanDuration as _;
use joinery::JoinableIterator as _;

#[derive(Debug)]
pub enum NewJob {
    Download {
        url: url::Url,
    },
    Unarchive,
    Process {
        status: ProcessStatus,
    },
    RegistryFetch {
        total_blobs: usize,
        total_recipes: usize,
    },
}

#[derive(Debug)]
pub enum UpdateJob {
    Download {
        progress_percent: Option<u8>,
    },
    Unarchive {
        progress_percent: u8,
    },
    Process {
        packet: DebugIgnore<Option<ProcessPacket>>,
        status: ProcessStatus,
    },
    RegistryFetchAdd {
        blobs_fetched: usize,
        recipes_fetched: usize,
    },
    RegistryFetchUpdate {
        total_blobs: Option<usize>,
        total_recipes: Option<usize>,
        complete_blobs: Option<usize>,
        complete_recipes: Option<usize>,
    },
    RegistryFetchFinish,
}

#[derive(Debug)]
pub enum Job {
    Download {
        url: url::Url,
        progress_percent: Option<u8>,
    },
    Unarchive {
        progress_percent: u8,
    },
    Process {
        packet_queue: DebugIgnore<Arc<RwLock<Vec<ProcessPacket>>>>,
        status: ProcessStatus,
    },
    RegistryFetch {
        complete_blobs: usize,
        total_blobs: usize,
        complete_recipes: usize,
        total_recipes: usize,
    },
}

impl Job {
    pub fn new(new: NewJob) -> Self {
        match new {
            NewJob::Download { url } => Self::Download {
                url,
                progress_percent: Some(0),
            },
            NewJob::Unarchive => Self::Unarchive {
                progress_percent: 0,
            },
            NewJob::Process { status } => Self::Process {
                packet_queue: Default::default(),
                status,
            },
            NewJob::RegistryFetch {
                total_blobs,
                total_recipes,
            } => Self::RegistryFetch {
                complete_blobs: 0,
                total_blobs,
                complete_recipes: 0,
                total_recipes,
            },
        }
    }

    pub fn update(&mut self, update: UpdateJob) -> anyhow::Result<()> {
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
            UpdateJob::Unarchive {
                progress_percent: new_progress_percent,
            } => {
                let Self::Unarchive {
                    progress_percent, ..
                } = self
                else {
                    anyhow::bail!("tried to update a non-unarchive job with an unarchive update");
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
            UpdateJob::RegistryFetchAdd {
                blobs_fetched,
                recipes_fetched,
            } => {
                let Self::RegistryFetch {
                    complete_blobs,
                    complete_recipes,
                    ..
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-registry-fetch job with a registry-fetch update"
                    );
                };

                *complete_blobs += blobs_fetched;
                *complete_recipes += recipes_fetched;
            }
            UpdateJob::RegistryFetchUpdate {
                total_blobs: new_total_blobs,
                total_recipes: new_total_recipes,
                complete_blobs: new_complete_blobs,
                complete_recipes: new_complete_recipes,
            } => {
                let Self::RegistryFetch {
                    total_blobs,
                    total_recipes,
                    complete_blobs,
                    complete_recipes,
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-registry-fetch job with a registry-fetch update"
                    );
                };

                if let Some(new_total_blobs) = new_total_blobs {
                    *total_blobs = new_total_blobs;
                }
                if let Some(new_total_recipes) = new_total_recipes {
                    *total_recipes = new_total_recipes;
                }
                if let Some(new_complete_blobs) = new_complete_blobs {
                    *complete_blobs = new_complete_blobs;
                }
                if let Some(new_complete_recipes) = new_complete_recipes {
                    *complete_recipes = new_complete_recipes;
                }
            }
            UpdateJob::RegistryFetchFinish => {
                let Self::RegistryFetch {
                    complete_blobs,
                    total_blobs,
                    complete_recipes,
                    total_recipes,
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-registry-fetch job with a registry-fetch-finish update"
                    );
                };

                *complete_blobs = *total_blobs;
                *complete_recipes = *total_recipes;
            }
        }

        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        match self {
            Job::Download {
                progress_percent, ..
            } => progress_percent.map(|p| p >= 100).unwrap_or(false),
            Job::Unarchive { progress_percent } => *progress_percent >= 100,
            Job::Process {
                status,
                packet_queue: _,
            } => matches!(status, ProcessStatus::Exited { .. }),
            Job::RegistryFetch {
                complete_blobs,
                total_blobs,
                complete_recipes,
                total_recipes,
            } => total_blobs == complete_blobs && total_recipes == complete_recipes,
        }
    }

    // Returns a priority for the job type. 0 is the lowest priority. Higher
    // priority jobs are displayed first.
    pub fn job_type_priority(&self) -> u8 {
        match self {
            Job::Unarchive { .. } => 0,
            Job::Download { .. } | Job::RegistryFetch { .. } => 1,
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
            Job::Unarchive { progress_percent } => {
                let message = if *progress_percent == 100 {
                    "[100%] Unarchived".to_string()
                } else {
                    format!("[{progress_percent:>3}%] Unarchiving")
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
            Job::RegistryFetch {
                complete_blobs,
                total_blobs,
                complete_recipes,
                total_recipes,
            } => {
                let blob_percent = if *total_blobs > 0 {
                    (*complete_blobs as f64 / *total_blobs as f64) * 100.0
                } else {
                    100.0
                };
                let recipe_percent = if *total_recipes > 0 {
                    (*complete_recipes as f64 / *total_recipes as f64) * 100.0
                } else {
                    100.0
                };
                let total_percent = (recipe_percent * 0.2 + blob_percent * 0.8) as u8;
                let verb = if self.is_complete() {
                    "Fetched"
                } else {
                    "Fetching"
                };
                let fetched_blobs = if *total_blobs == 0 {
                    None
                } else if self.is_complete() {
                    Some(format!(
                        "{complete_blobs} blob{s}",
                        s = if *complete_blobs == 1 { "" } else { "s" }
                    ))
                } else {
                    Some(format!(
                        "{complete_blobs} / {total_blobs} blob{s}",
                        s = if *total_blobs == 1 { "" } else { "s" }
                    ))
                };
                let fetched_recipes = if *total_recipes == 0 {
                    None
                } else if self.is_complete() {
                    Some(format!(
                        "{complete_recipes} recipe{s}",
                        s = if *complete_recipes == 1 { "" } else { "s" }
                    ))
                } else {
                    Some(format!(
                        "{complete_recipes} / {total_recipes} recipe{s}",
                        s = if *total_recipes == 1 { "" } else { "s" }
                    ))
                };
                let fetching_message = [fetched_blobs, fetched_recipes]
                    .into_iter()
                    .flatten()
                    .join_with(" + ");
                let message =
                    format!("[{total_percent:>3}%] {verb} {fetching_message} from registry",);
                superconsole::Lines::from_iter([superconsole::Line::sanitized(&message)])
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
    pub fn child_id(&self) -> Option<u32> {
        match self {
            ProcessStatus::Running { child_id, .. } => *child_id,
            ProcessStatus::Exited { child_id, .. } => *child_id,
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        match self {
            ProcessStatus::Running { start, .. } => start.elapsed(),
            ProcessStatus::Exited { elapsed, .. } => *elapsed,
        }
    }
}
