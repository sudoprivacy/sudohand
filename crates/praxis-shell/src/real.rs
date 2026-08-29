//! The real backend over `std::process::Command`.
//!
//! Robustness rules (each one is a hang or a leak we hit otherwise):
//! - The child runs in its **own process group**; a timeout kills the whole
//!   group, so a `sleep 30 &` grandchild cannot outlive the command or keep
//!   our stdout pipe open.
//! - stdin is written on its own thread; a child that never reads cannot
//!   block the timeout.
//! - After the child exits, output is drained until EOF or a short grace
//!   period; if something else is still holding the pipes (a backgrounded
//!   grandchild), the group is killed and the result is returned anyway.
//! - `via_shell` uses `/bin/sh`, not `sh` looked up through a possibly
//!   overridden `PATH`.

// The one unsafe call in this crate: `kill(-pgid)` to reap the process group.
#![allow(unsafe_code)]

use crate::backend::{RunRequest, RunResult, ShellBackend};
use praxis_core::{Error, Result};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct RealShell;

impl RealShell {
    pub fn new() -> Self {
        Self
    }
}

/// How long to keep reading after the child exited before giving up on
/// pipes still held open by its descendants.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Read a stream to the end on a thread, keeping at most `cap` bytes; the
/// result is delivered over a channel so the caller can wait with a deadline.
fn drain(mut r: impl Read + Send + 'static, cap: Option<usize>) -> mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = mpsc::channel();
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
        let _ = tx.send((buf, truncated));
    });
    rx
}

fn recv_until(rx: &mpsc::Receiver<(Vec<u8>, bool)>, deadline: Instant) -> Option<(Vec<u8>, bool)> {
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
}

/// Kill the child's whole process group (unix), falling back to the child.
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    {
        // SAFETY: plain syscall on a pid we spawned into its own group.
        let pgid = child.id() as libc::pid_t;
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

fn spawn_error(what: &str, e: &std::io::Error) -> Error {
    let msg = format!("spawn {what}: {e}");
    match Error::from_io(e) {
        Error::NotFound(_) => Error::not_found(msg),
        Error::PermissionDenied(_) => Error::perm(msg),
        _ => Error::io(msg),
    }
}

impl ShellBackend for RealShell {
    fn run(&self, req: &RunRequest) -> Result<RunResult> {
        if req.program.is_empty() {
            return Err(Error::invalid("empty program"));
        }
        if let Some(cwd) = &req.cwd {
            if !cwd.is_dir() {
                return Err(Error::not_found(format!(
                    "cwd {}: not a directory",
                    cwd.display()
                )));
            }
        }
        let (exe, mut cmd) = if req.via_shell {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(&req.program).arg("sh").args(&req.args);
            ("/bin/sh".to_string(), c)
        } else {
            let mut c = Command::new(&req.program);
            c.args(&req.args);
            (req.program.clone(), c)
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
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| spawn_error(&exe, &e))?;

        if let (Some(data), Some(mut stdin)) = (req.stdin.clone(), child.stdin.take()) {
            // Off-thread: a child that never reads must not block us. A
            // child that exits without reading yields EPIPE; not our error.
            std::thread::spawn(move || {
                let _ = stdin.write_all(&data);
            });
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
                kill_group(&mut child);
                break child.wait().map_err(|e| Error::io(e.to_string()))?;
            }
            std::thread::sleep(Duration::from_millis(5));
        };

        // Drain what is left. If descendants still hold the pipes after the
        // grace period, kill the group so nothing outlives the command.
        let grace = Instant::now() + DRAIN_GRACE;
        let mut stdout = recv_until(&out, grace);
        let mut stderr = recv_until(&err, grace);
        if stdout.is_none() || stderr.is_none() {
            kill_group(&mut child);
            let last = Instant::now() + DRAIN_GRACE;
            if stdout.is_none() {
                stdout = recv_until(&out, last);
            }
            if stderr.is_none() {
                stderr = recv_until(&err, last);
            }
        }
        let (stdout, stdout_truncated) = stdout.unwrap_or_default();
        let (stderr, stderr_truncated) = stderr.unwrap_or_default();

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
