use std::sync::{Arc, RwLock};

use debug_ignore::DebugIgnore;

#[derive(Debug)]
pub enum NewJob {
    Download {
        url: url::Url,
        started_at: std::time::Instant,
    },
    Unarchive {
        started_at: std::time::Instant,
    },
    Process {
        status: ProcessStatus,
    },
    CacheFetch {
        kind: CacheFetchKind,
        downloaded_data: Option<u64>,
        total_data: Option<u64>,
        downloaded_blobs: Option<u64>,
        total_blobs: Option<u64>,
        started_at: std::time::Instant,
    },
}

#[derive(Debug)]
pub enum UpdateJob {
    Download {
        progress_percent: Option<u8>,
        finished_at: Option<std::time::Instant>,
    },
    Unarchive {
        progress_percent: u8,
        finished_at: Option<std::time::Instant>,
    },
    ProcessPushPacket {
        packet: DebugIgnore<ProcessPacket>,
    },
    ProcessFlushPackets,
    ProcessUpdateStatus {
        status: ProcessStatus,
    },
    CacheFetchAdd {
        downloaded_data: Option<u64>,
        downloaded_blobs: Option<u64>,
    },
    CacheFetchUpdate {
        downloaded_data: Option<u64>,
        total_data: Option<u64>,
        downloaded_blobs: Option<u64>,
        total_blobs: Option<u64>,
    },
    CacheFetchFinish {
        finished_at: std::time::Instant,
    },
}

#[derive(Debug)]
pub enum Job {
    Download {
        url: url::Url,
        progress_percent: Option<u8>,
        started_at: std::time::Instant,
        finished_at: Option<std::time::Instant>,
    },
    Unarchive {
        progress_percent: u8,
        started_at: std::time::Instant,
        finished_at: Option<std::time::Instant>,
    },
    Process {
        packet_queue: DebugIgnore<Arc<RwLock<Vec<ProcessPacket>>>>,
        status: ProcessStatus,
    },
    CacheFetch {
        kind: CacheFetchKind,
        downloaded_data: u64,
        total_data: Option<u64>,
        downloaded_blobs: u64,
        total_blobs: Option<u64>,
        started_at: std::time::Instant,
        finished_at: Option<std::time::Instant>,
    },
}

impl Job {
    pub fn new(new: NewJob) -> Self {
        match new {
            NewJob::Download { url, started_at } => Self::Download {
                url,
                progress_percent: Some(0),
                started_at,
                finished_at: None,
            },
            NewJob::Unarchive { started_at } => Self::Unarchive {
                progress_percent: 0,
                started_at,
                finished_at: None,
            },
            NewJob::Process { status } => Self::Process {
                packet_queue: Default::default(),
                status,
            },
            NewJob::CacheFetch {
                kind,
                downloaded_data,
                total_data,
                downloaded_blobs,
                total_blobs,
                started_at,
            } => Self::CacheFetch {
                kind,
                downloaded_data: downloaded_data.unwrap_or(0),
                total_data,
                downloaded_blobs: downloaded_blobs.unwrap_or(0),
                total_blobs,
                started_at,
                finished_at: None,
            },
        }
    }

    pub fn update(&mut self, update: UpdateJob) -> anyhow::Result<()> {
        match update {
            UpdateJob::Download {
                progress_percent: new_progress_percent,
                finished_at: new_finished_at,
            } => {
                let Self::Download {
                    progress_percent,
                    finished_at,
                    ..
                } = self
                else {
                    anyhow::bail!("tried to update a non-download job with a download update");
                };
                *progress_percent = new_progress_percent;
                *finished_at = new_finished_at;
            }
            UpdateJob::Unarchive {
                progress_percent: new_progress_percent,
                finished_at: new_finished_at,
            } => {
                let Self::Unarchive {
                    progress_percent,
                    finished_at,
                    ..
                } = self
                else {
                    anyhow::bail!("tried to update a non-unarchive job with an unarchive update");
                };
                *progress_percent = new_progress_percent;
                *finished_at = new_finished_at;
            }
            UpdateJob::ProcessPushPacket { packet } => {
                let Self::Process {
                    packet_queue,
                    status: _,
                } = self
                else {
                    anyhow::bail!("tried to update a non-process job with a process update");
                };

                let mut packet_queue = packet_queue.write().map_err(|_| {
                    anyhow::anyhow!("failed to lock process packet queue for writing")
                })?;
                packet_queue.push(packet.0);
            }
            UpdateJob::ProcessFlushPackets => {}
            UpdateJob::ProcessUpdateStatus { status: new_status } => {
                let Self::Process {
                    packet_queue: _,
                    status,
                } = self
                else {
                    anyhow::bail!("tried to update a non-process job with a process update");
                };

                *status = new_status;
            }
            UpdateJob::CacheFetchAdd {
                downloaded_data: add_downloaded_data,
                downloaded_blobs: add_downloaded_blobs,
            } => {
                let Self::CacheFetch {
                    downloaded_data,
                    downloaded_blobs,
                    ..
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-cache-fetch job with a cache-fetch update"
                    );
                };

                if let Some(add_downloaded_data) = add_downloaded_data {
                    *downloaded_data += add_downloaded_data;
                }

                if let Some(add_downloaded_blobs) = add_downloaded_blobs {
                    *downloaded_blobs += add_downloaded_blobs;
                }
            }
            UpdateJob::CacheFetchUpdate {
                downloaded_data: new_downloaded_data,
                total_data: new_total_data,
                downloaded_blobs: new_downloaded_blobs,
                total_blobs: new_total_blobs,
            } => {
                let Self::CacheFetch {
                    kind: _,
                    downloaded_data,
                    total_data,
                    downloaded_blobs,
                    total_blobs,
                    started_at: _,
                    finished_at: _,
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-cache-fetch job with a cache-fetch update"
                    );
                };

                if let Some(new_downloaded_data) = new_downloaded_data {
                    *downloaded_data = new_downloaded_data;
                }
                if let Some(new_total_data) = new_total_data {
                    *total_data = Some(new_total_data);
                }
                if let Some(new_downloaded_blobs) = new_downloaded_blobs {
                    *downloaded_blobs = new_downloaded_blobs;
                }
                if let Some(new_total_blobs) = new_total_blobs {
                    *total_blobs = Some(new_total_blobs);
                }
            }
            UpdateJob::CacheFetchFinish {
                finished_at: new_finished_at,
            } => {
                let Self::CacheFetch {
                    kind: _,
                    downloaded_data,
                    total_data,
                    downloaded_blobs,
                    total_blobs,
                    started_at: _,
                    finished_at,
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-cache-fetch job with a cache-fetch-finish update"
                    );
                };

                if let Some(total_data) = total_data {
                    *downloaded_data = *total_data;
                }

                if let Some(total_blobs) = total_blobs {
                    *downloaded_blobs = *total_blobs;
                }

                *finished_at = Some(new_finished_at);
            }
        }

        Ok(())
    }

    pub fn created_at(&self) -> std::time::Instant {
        match self {
            Job::Download { started_at, .. }
            | Job::Unarchive { started_at, .. }
            | Job::CacheFetch { started_at, .. } => *started_at,
            Job::Process { status, .. } => status.created_at(),
        }
    }

    pub fn started_at(&self) -> Option<std::time::Instant> {
        match self {
            Job::Download { started_at, .. }
            | Job::Unarchive { started_at, .. }
            | Job::CacheFetch { started_at, .. } => Some(*started_at),
            Job::Process { status, .. } => status.started_at(),
        }
    }

    pub fn finished_at(&self) -> Option<std::time::Instant> {
        match self {
            Job::Download { finished_at, .. }
            | Job::Unarchive { finished_at, .. }
            | Job::CacheFetch { finished_at, .. } => *finished_at,
            Job::Process { status, .. } => status.finished_at(),
        }
    }

    pub fn finalized_at(&self) -> Option<std::time::Instant> {
        match self {
            Job::Download { finished_at, .. }
            | Job::Unarchive { finished_at, .. }
            | Job::CacheFetch { finished_at, .. } => *finished_at,
            Job::Process { status, .. } => status.finalized_at(),
        }
    }

    pub fn elapsed(&self) -> Option<std::time::Duration> {
        let started_at = self.started_at()?;
        let elapsed = if let Some(finished_at) = self.finished_at() {
            finished_at.saturating_duration_since(started_at)
        } else {
            started_at.elapsed()
        };
        Some(elapsed)
    }

    pub fn is_complete(&self) -> bool {
        self.finished_at().is_some()
    }

    // Returns a priority for the job type. 0 is the lowest priority. Higher
    // priority jobs are displayed first.
    pub fn job_type_priority(&self) -> u8 {
        match self {
            Job::Unarchive { .. } => 0,
            Job::Download { .. } | Job::CacheFetch { .. } | Job::Process { .. } => 2,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CacheFetchKind {
    Bake,
    Project,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProcessStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub enum ProcessStatus {
    Preparing {
        created_at: std::time::Instant,
    },
    Running {
        child_id: Option<u32>,
        created_at: std::time::Instant,
        started_at: std::time::Instant,
    },
    Ran {
        child_id: Option<u32>,
        created_at: std::time::Instant,
        started_at: std::time::Instant,
        finished_at: std::time::Instant,
    },
    Finalized {
        child_id: Option<u32>,
        created_at: std::time::Instant,
        started_at: std::time::Instant,
        finished_at: std::time::Instant,
        finalized_at: std::time::Instant,
    },
}

impl ProcessStatus {
    fn created_at(&self) -> std::time::Instant {
        match self {
            Self::Preparing { created_at }
            | Self::Running { created_at, .. }
            | Self::Ran { created_at, .. }
            | Self::Finalized { created_at, .. } => *created_at,
        }
    }

    fn started_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Preparing { .. } => None,
            Self::Running { started_at, .. }
            | Self::Ran { started_at, .. }
            | Self::Finalized { started_at, .. } => Some(*started_at),
        }
    }

    fn finished_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Preparing { .. } | Self::Running { .. } => None,
            Self::Ran { finished_at, .. } | Self::Finalized { finished_at, .. } => {
                Some(*finished_at)
            }
        }
    }

    fn finalized_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Preparing { .. } | Self::Running { .. } | Self::Ran { .. } => None,
            Self::Finalized { finalized_at, .. } => Some(*finalized_at),
        }
    }

    pub fn child_id(&self) -> Option<u32> {
        match self {
            ProcessStatus::Preparing { .. } => None,
            ProcessStatus::Running { child_id, .. }
            | ProcessStatus::Ran { child_id, .. }
            | ProcessStatus::Finalized { child_id, .. } => *child_id,
        }
    }

    pub fn to_running(
        &mut self,
        started_at: std::time::Instant,
        child_id: Option<u32>,
    ) -> anyhow::Result<()> {
        let Self::Preparing { created_at } = *self else {
            anyhow::bail!("expected ProcessStatus to be Preparing");
        };

        *self = Self::Running {
            child_id,
            created_at,
            started_at,
        };

        Ok(())
    }

    pub fn to_ran(&mut self, finished_at: std::time::Instant) -> anyhow::Result<()> {
        let Self::Running {
            child_id,
            created_at,
            started_at,
        } = *self
        else {
            anyhow::bail!("expected ProcessStatus to be Running");
        };

        *self = Self::Ran {
            child_id,
            created_at,
            started_at,
            finished_at,
        };

        Ok(())
    }

    pub fn to_finalized(&mut self, finalized_at: std::time::Instant) -> anyhow::Result<()> {
        let Self::Ran {
            child_id,
            created_at,
            started_at,
            finished_at,
        } = *self
        else {
            anyhow::bail!("expected ProcessStatus to be Ran");
        };

        *self = Self::Finalized {
            child_id,
            created_at,
            started_at,
            finished_at,
            finalized_at,
        };

        Ok(())
    }
}
