//! The value types and the [`ShellBackend`] trait.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use sudohand_core::Result;

/// What to run. `program` + `args` are executed directly (no shell
/// interpretation) unless `via_shell` is set, in which case `program` is a
/// script handed to `sh -c` (and `args` become `$1..`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RunRequest {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub via_shell: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Extra environment variables (added to the inherited environment).
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Bytes fed to the child's stdin (stdin is closed either way).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<Vec<u8>>,
    /// Kill the child after this many milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Cap on captured bytes per stream (`None` = unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<usize>,
}

/// What happened. `stdout`/`stderr` are lossily decoded as UTF-8.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RunResult {
    /// `None` when the child was killed by a signal (or timed out).
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Whether the per-stream cap cut the output.
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub duration_ms: u64,
}

impl RunResult {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }
}

/// Synchronous; blocks until the child exits or the timeout fires. No
/// allow-list, cwd fence or confirmation here — the integrator owns those.
pub trait ShellBackend: Send + Sync + std::fmt::Debug {
    fn run(&self, req: &RunRequest) -> Result<RunResult>;
}
