use chrono::{Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Lifecycle recorded in a durable job snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

fn unlimited() -> i32 {
    -1
}

pub(super) fn timestamp() -> NaiveDateTime {
    let now = Local::now().naive_local();
    now.with_nanosecond(now.nanosecond() / 1_000 * 1_000)
        .expect("microsecond precision is a valid nanosecond value")
}

fn error_bases<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error> {
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

/// A JSON-serializable invocation. A Rust executor dispatches `task_type`;
/// positional and named arguments retain the reference's state-file shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub task_type: String,
    #[serde(default)]
    pub args: Vec<Value>,
    #[serde(default)]
    pub kwargs: Map<String, Value>,
    #[serde(default)]
    pub retries: u32,
    /// Reference attempt budget, or -1 for unlimited retries. The scheduler
    /// increments `retries` after a failure, then compares it to this budget.
    #[serde(default = "unlimited")]
    pub max_retries: i32,
    pub created_at: NaiveDateTime,
    #[serde(default)]
    pub status: JobStatus,
}

impl Job {
    #[must_use]
    pub fn new(task_type: impl Into<String>) -> Self {
        Self {
            job_id: uuid::Uuid::new_v4().to_string(),
            task_type: task_type.into(),
            args: Vec::new(),
            kwargs: Map::new(),
            retries: 0,
            max_retries: -1,
            created_at: timestamp(),
            status: JobStatus::Pending,
        }
    }

    #[must_use]
    pub fn can_retry(&self) -> bool {
        self.max_retries == -1 || i64::from(self.retries) < i64::from(self.max_retries)
    }
}

/// Outcome with structured error identity, allowing retry policy without parsing
/// an error message. Rust executors supply error type and ancestry explicitly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobResult {
    pub job_id: String,
    pub success: bool,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default, deserialize_with = "error_bases")]
    pub error_bases: Vec<String>,
    pub completed_at: NaiveDateTime,
    #[serde(default)]
    pub worker_id: Option<u64>,
}

impl JobResult {
    #[must_use]
    pub fn success(job_id: impl Into<String>, data: Value, worker_id: Option<u64>) -> Self {
        Self {
            job_id: job_id.into(),
            success: true,
            data,
            error: None,
            error_type: None,
            error_bases: Vec::new(),
            completed_at: timestamp(),
            worker_id,
        }
    }

    #[must_use]
    pub fn failure(
        job_id: impl Into<String>,
        message: impl Into<String>,
        error_type: impl Into<String>,
        error_bases: Vec<String>,
        worker_id: Option<u64>,
    ) -> Self {
        Self {
            success: false,
            error: Some(message.into()),
            error_type: Some(error_type.into()),
            error_bases,
            ..Self::success(job_id, Value::Null, worker_id)
        }
    }
}
