use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Job;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    #[default]
    Idle,
    Busy,
    Stopping,
    Stopped,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerStats {
    pub success: u64,
    pub fail: u64,
    pub total_time: f64,
}

impl WorkerStats {
    #[must_use]
    pub fn total(&self) -> u64 {
        self.success + self.fail
    }

    #[must_use]
    pub fn success_rate(&self) -> f64 {
        if self.total() == 0 {
            0.0
        } else {
            self.success as f64 / self.total() as f64
        }
    }
}

/// Observable worker state. Its client and execution task are owned by the
/// scheduler, so callers can inspect a snapshot without holding a browser lock.
#[derive(Clone, Debug)]
pub struct Worker {
    pub worker_id: u64,
    pub port: u16,
    pub status: WorkerStatus,
    pub current_job: Option<Job>,
    pub stats: WorkerStats,
}

impl Worker {
    #[must_use]
    pub fn new(worker_id: u64, port: u16) -> Self {
        Self {
            worker_id,
            port,
            status: WorkerStatus::Idle,
            current_job: None,
            stats: WorkerStats::default(),
        }
    }

    #[must_use]
    pub fn to_dict(&self) -> Value {
        json!({
            "worker_id": self.worker_id,
            "port": self.port,
            "status": self.status,
            "current_job_id": self.current_job.as_ref().map(|job| &job.job_id),
            "stats": {
                "success": self.stats.success,
                "fail": self.stats.fail,
                "total": self.stats.total(),
                "success_rate": (self.stats.success_rate() * 100.0).round_ties_even() / 100.0,
            }
        })
    }
}
