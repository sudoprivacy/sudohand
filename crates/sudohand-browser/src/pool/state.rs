use std::{collections::BTreeMap, io::Write, path::Path};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use super::{Job, JobResult, JobStatus};
use crate::Result;

fn version() -> u32 {
    1
}

/// Version-one checkpoint. Completed results are retained across restarts;
/// pending and interrupted jobs are eligible for resumption.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoolState {
    #[serde(default = "version")]
    pub version: u32,
    pub last_updated: NaiveDateTime,
    #[serde(default)]
    pub completed: BTreeMap<String, JobResult>,
    #[serde(default)]
    pub pending: Vec<Job>,
    #[serde(default)]
    pub in_progress: Vec<Job>,
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            version: version(),
            last_updated: super::model::timestamp(),
            completed: BTreeMap::new(),
            pending: Vec::new(),
            in_progress: Vec::new(),
        }
    }
}

impl PoolState {
    /// Restore queue order as pending jobs followed by interrupted jobs. Preserve
    /// IDs, retry budgets and completed results; never rerun completed work.
    pub fn resume(&mut self) {
        self.pending.append(&mut self.in_progress);
        // The reference can put the same active job in both lists.
        let mut seen = std::collections::HashSet::new();
        self.pending.retain(|job| {
            !self.completed.contains_key(&job.job_id) && seen.insert(job.job_id.clone())
        });
        for job in &mut self.pending {
            job.status = JobStatus::Pending;
        }
    }
}

/// Same-directory temporary file and atomic replacement. Sync the new file before
/// making it visible. Failed writes leave the previous checkpoint intact.
pub fn save_state(state: &mut PoolState, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut updated = state.clone();
    updated.last_updated = super::model::timestamp();
    let mut temporary = tempfile::Builder::new()
        .prefix(".pool_state_")
        .tempfile_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), &updated)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    state.last_updated = updated.last_updated;
    Ok(())
}

/// Missing or malformed checkpoints return `None`, matching the reference.
/// Filesystem errors (such as permission failures) remain actionable errors.
pub fn load_state(path: impl AsRef<Path>) -> Result<Option<PoolState>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(serde_json::from_slice(&bytes).ok())
}
