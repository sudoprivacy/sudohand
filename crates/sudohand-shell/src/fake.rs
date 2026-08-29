//! A backend that never spawns anything: it records each request and
//! answers from a table of canned results (default: exit 0, empty output).

use crate::backend::{RunRequest, RunResult, ShellBackend};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sudohand_core::Result;

#[derive(Debug, Default)]
pub struct FakeShell {
    pub requests: Mutex<Vec<RunRequest>>,
    canned: Mutex<HashMap<String, RunResult>>,
}

impl FakeShell {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// Answer `program` with `result` (keyed on the program / script only).
    pub fn with_canned(self: Arc<Self>, program: &str, result: RunResult) -> Arc<Self> {
        self.canned
            .lock()
            .unwrap()
            .insert(program.to_string(), result);
        self
    }
    pub fn requests(&self) -> Vec<RunRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl ShellBackend for FakeShell {
    fn run(&self, req: &RunRequest) -> Result<RunResult> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(self
            .canned
            .lock()
            .unwrap()
            .get(&req.program)
            .cloned()
            .unwrap_or(RunResult {
                exit_code: Some(0),
                ..Default::default()
            }))
    }
}
