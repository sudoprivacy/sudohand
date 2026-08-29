//! The real backend over `std::process::Command`.

use crate::backend::{RunRequest, RunResult, ShellBackend};
use praxis_core::{Error, Result};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct RealShell;

impl RealShell {
    pub fn new() -> Self {
        Self
    }
}

/// Read a stream to the end on a thread, keeping at most `cap` bytes.
fn drain(
    mut r: impl Read + Send + 'static,
    cap: Option<usize>,
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut truncated = false;
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(cap) = cap {
                        let room = cap.saturating_sub(buf.len());
                        if n > room {
                            truncated = true;
                        }
                        buf.extend_from_slice(&chunk[..n.min(room)]);
                    } else {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        (buf, truncated)
    })
}

impl ShellBackend for RealShell {
    fn run(&self, req: &RunRequest) -> Result<RunResult> {
        if req.program.is_empty() {
            return Err(Error::invalid("empty program"));
        }
        let mut cmd = if req.via_shell {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&req.program).arg("sh").args(&req.args);
            c
        } else {
            let mut c = Command::new(&req.program);
            c.args(&req.args);
            c
        };
        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }
        cmd.envs(req.env.iter().map(|(k, v)| (k, v)));
        cmd.stdin(if req.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| {
            let msg = format!("spawn {}: {e}", req.program);
            match Error::from_io(&e) {
                Error::NotFound(_) => Error::not_found(msg),
                Error::PermissionDenied(_) => Error::perm(msg),
                _ => Error::io(msg),
            }
        })?;

        if let (Some(data), Some(mut stdin)) = (&req.stdin, child.stdin.take()) {
            // A child that exits without reading stdin yields EPIPE; that is
            // not our error.
            let _ = stdin.write_all(data);
        }
        let out = drain(
            child.stdout.take().expect("piped stdout"),
            req.max_output_bytes,
        );
        let err = drain(
            child.stderr.take().expect("piped stderr"),
            req.max_output_bytes,
        );

        let deadline = req.timeout_ms.map(|ms| started + Duration::from_millis(ms));
        let mut timed_out = false;
        let status = loop {
            if let Some(st) = child.try_wait().map_err(|e| Error::io(e.to_string()))? {
                break st;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                timed_out = true;
                let _ = child.kill();
                break child.wait().map_err(|e| Error::io(e.to_string()))?;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let (stdout, stdout_truncated) = out
            .join()
            .map_err(|_| Error::internal("stdout reader panicked"))?;
        let (stderr, stderr_truncated) = err
            .join()
            .map_err(|_| Error::internal("stderr reader panicked"))?;

        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;

        Ok(RunResult {
            exit_code: status.code(),
            signal,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            stdout_truncated,
            stderr_truncated,
            timed_out,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}
