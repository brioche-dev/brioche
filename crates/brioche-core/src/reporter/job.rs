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
        total_bytes: u64,
    },
    Process {
        status: ProcessStatus,
    },
    CacheFetch {
        kind: CacheFetchKind,
        downloaded_bytes: Option<u64>,
        total_bytes: Option<u64>,
        started_at: std::time::Instant,
    },
}

#[derive(Debug)]
pub enum UpdateJob {
    Download {
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        finished_at: Option<std::time::Instant>,
    },
    Unarchive {
        read_bytes: u64,
        total_bytes: Option<u64>,
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
        downloaded_bytes: Option<u64>,
    },
    CacheFetchUpdate {
        downloaded_bytes: Option<u64>,
        total_bytes: Option<u64>,
    },
    CacheFetchFinish {
        finished_at: std::time::Instant,
    },
}

#[derive(Debug)]
pub enum Job {
    Download {
        url: url::Url,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        started_at: std::time::Instant,
        finished_at: Option<std::time::Instant>,
    },
    Unarchive {
        read_bytes: u64,
        total_bytes: u64,
        started_at: std::time::Instant,
        finished_at: Option<std::time::Instant>,
    },
    Process {
        packet_queue: DebugIgnore<Arc<RwLock<Vec<ProcessPacket>>>>,
        status: ProcessStatus,
    },
    CacheFetch {
        kind: CacheFetchKind,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        started_at: std::time::Instant,
        finished_at: Option<std::time::Instant>,
    },
}

impl Job {
    pub fn new(new: NewJob) -> Self {
        match new {
            NewJob::Download { url, started_at } => Self::Download {
                url,
                downloaded_bytes: 0,
                total_bytes: None,
                started_at,
                finished_at: None,
            },
            NewJob::Unarchive {
                started_at,
                total_bytes,
            } => Self::Unarchive {
                read_bytes: 0,
                total_bytes,
                started_at,
                finished_at: None,
            },
            NewJob::Process { status } => Self::Process {
                packet_queue: Default::default(),
                status,
            },
            NewJob::CacheFetch {
                kind,
                downloaded_bytes,
                total_bytes,
                started_at,
            } => Self::CacheFetch {
                kind,
                downloaded_bytes: downloaded_bytes.unwrap_or(0),
                total_bytes,
                started_at,
                finished_at: None,
            },
        }
    }

    pub fn update(&mut self, update: UpdateJob) -> anyhow::Result<()> {
        match update {
            UpdateJob::Download {
                downloaded_bytes: new_downloaded_bytes,
                total_bytes: new_total_bytes,
                finished_at: new_finished_at,
            } => {
                let Self::Download {
                    downloaded_bytes,
                    total_bytes,
                    finished_at,
                    ..
                } = self
                else {
                    anyhow::bail!("tried to update a non-download job with a download update");
                };
                *downloaded_bytes = new_downloaded_bytes;
                *total_bytes = new_total_bytes.or(*total_bytes);
                *finished_at = new_finished_at;
            }
            UpdateJob::Unarchive {
                read_bytes: new_read_bytes,
                total_bytes: new_total_bytes,
                finished_at: new_finished_at,
            } => {
                let Self::Unarchive {
                    read_bytes,
                    total_bytes,
                    finished_at,
                    ..
                } = self
                else {
                    anyhow::bail!("tried to update a non-unarchive job with an unarchive update");
                };
                *read_bytes = new_read_bytes;
                *total_bytes = new_total_bytes.unwrap_or(*total_bytes);
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
                downloaded_bytes: add_downloaded_bytes,
            } => {
                let Self::CacheFetch {
                    downloaded_bytes, ..
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-cache-fetch job with a cache-fetch update"
                    );
                };

                if let Some(add_downloaded_bytes) = add_downloaded_bytes {
                    *downloaded_bytes += add_downloaded_bytes;
                }
            }
            UpdateJob::CacheFetchUpdate {
                downloaded_bytes: new_downloaded_bytes,
                total_bytes: new_total_bytes,
            } => {
                let Self::CacheFetch {
                    kind: _,
                    downloaded_bytes,
                    total_bytes,
                    started_at: _,
                    finished_at: _,
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-cache-fetch job with a cache-fetch update"
                    );
                };

                if let Some(new_downloaded_bytes) = new_downloaded_bytes {
                    *downloaded_bytes = new_downloaded_bytes;
                }
                if let Some(new_total_bytes) = new_total_bytes {
                    *total_bytes = Some(new_total_bytes);
                }
            }
            UpdateJob::CacheFetchFinish {
                finished_at: new_finished_at,
            } => {
                let Self::CacheFetch {
                    kind: _,
                    downloaded_bytes,
                    total_bytes,
                    started_at: _,
                    finished_at,
                } = self
                else {
                    anyhow::bail!(
                        "tried to update a non-cache-fetch job with a cache-fetch-finish update"
                    );
                };

                if let Some(total_bytes) = total_bytes {
                    *downloaded_bytes = *total_bytes;
                }

                *finished_at = Some(new_finished_at);
            }
        }

        Ok(())
    }

    pub const fn created_at(&self) -> std::time::Instant {
        match self {
            Self::Download { started_at, .. }
            | Self::Unarchive { started_at, .. }
            | Self::CacheFetch { started_at, .. } => *started_at,
            Self::Process { status, .. } => status.created_at(),
        }
    }

    pub const fn started_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Download { started_at, .. }
            | Self::Unarchive { started_at, .. }
            | Self::CacheFetch { started_at, .. } => Some(*started_at),
            Self::Process { status, .. } => status.started_at(),
        }
    }

    pub const fn finished_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Download { finished_at, .. }
            | Self::Unarchive { finished_at, .. }
            | Self::CacheFetch { finished_at, .. } => *finished_at,
            Self::Process { status, .. } => status.finished_at(),
        }
    }

    pub const fn finalized_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Download { finished_at, .. }
            | Self::Unarchive { finished_at, .. }
            | Self::CacheFetch { finished_at, .. } => *finished_at,
            Self::Process { status, .. } => status.finalized_at(),
        }
    }

    pub fn elapsed(&self) -> Option<std::time::Duration> {
        let started_at = self.started_at()?;
        let elapsed = self.finished_at().map_or_else(
            || started_at.elapsed(),
            |finished_at| finished_at.saturating_duration_since(started_at),
        );
        Some(elapsed)
    }

    pub const fn is_complete(&self) -> bool {
        self.finished_at().is_some()
    }

    // Returns a priority for the job type. 0 is the lowest priority. Higher
    // priority jobs are displayed first.
    pub const fn job_type_priority(&self) -> u8 {
        match self {
            Self::Unarchive { .. } => 0,
            Self::Download { .. } | Self::CacheFetch { .. } | Self::Process { .. } => 2,
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
    const fn created_at(&self) -> std::time::Instant {
        match self {
            Self::Preparing { created_at }
            | Self::Running { created_at, .. }
            | Self::Ran { created_at, .. }
            | Self::Finalized { created_at, .. } => *created_at,
        }
    }

    const fn started_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Preparing { .. } => None,
            Self::Running { started_at, .. }
            | Self::Ran { started_at, .. }
            | Self::Finalized { started_at, .. } => Some(*started_at),
        }
    }

    const fn finished_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Preparing { .. } | Self::Running { .. } => None,
            Self::Ran { finished_at, .. } | Self::Finalized { finished_at, .. } => {
                Some(*finished_at)
            }
        }
    }

    const fn finalized_at(&self) -> Option<std::time::Instant> {
        match self {
            Self::Preparing { .. } | Self::Running { .. } | Self::Ran { .. } => None,
            Self::Finalized { finalized_at, .. } => Some(*finalized_at),
        }
    }

    pub const fn child_id(&self) -> Option<u32> {
        match self {
            Self::Preparing { .. } => None,
            Self::Running { child_id, .. }
            | Self::Ran { child_id, .. }
            | Self::Finalized { child_id, .. } => *child_id,
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
