//! `suh shell run …` — thin CLI over `sudohand-shell`.
//!
//! Exposed here for operators and integrators that *choose* to link it;
//! see the crate docs for the red line (agents do not get this by default).

use clap::Subcommand;
use serde_json::{json, Value};
use sudohand_shell::{Result, RunRequest, ShellBackend};

#[derive(Subcommand)]
pub enum Cmd {
    /// Run a program: `suh shell run [flags] -- prog arg…`, or a script
    /// with `--sh 'cmd | cmd'`.
    Run {
        /// Interpret the single argument as a `sh -c` script.
        #[arg(long)]
        sh: bool,
        #[arg(long)]
        cwd: Option<String>,
        /// Extra environment, `KEY=VALUE` (repeatable).
        #[arg(long = "env")]
        envs: Vec<String>,
        /// Text fed to stdin.
        #[arg(long)]
        stdin: Option<String>,
        /// Kill the child after this many milliseconds.
        #[arg(long)]
        timeout_ms: Option<u64>,
        /// Cap captured bytes per stream (default 1 MiB; 0 = unlimited).
        #[arg(long, default_value_t = 1 << 20)]
        max_output: usize,
        /// The program and its arguments (put `--` before them).
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

pub fn run(cmd: Cmd) -> Result<Value> {
    run_with(&sudohand_shell::RealShell::with_signal_forwarding(), cmd)
}

pub fn run_with(sh: &dyn ShellBackend, cmd: Cmd) -> Result<Value> {
    let Cmd::Run {
        sh: via_shell,
        cwd,
        envs,
        stdin,
        timeout_ms,
        max_output,
        command,
    } = cmd;
    let mut env = Vec::new();
    for e in envs {
        let (k, v) = e.split_once('=').ok_or_else(|| {
            sudohand_shell::Error::invalid(format!("--env {e:?}: expected KEY=VALUE"))
        })?;
        env.push((k.to_string(), v.to_string()));
    }
    let mut it = command.into_iter();
    let program = it.next().unwrap_or_default();
    let req = RunRequest {
        program,
        args: it.collect(),
        via_shell,
        cwd: cwd.map(Into::into),
        env,
        stdin: stdin.map(String::into_bytes),
        timeout_ms,
        max_output_bytes: (max_output > 0).then_some(max_output),
    };
    let r = sh.run(&req)?;
    let mut v =
        serde_json::to_value(&r).map_err(|e| sudohand_shell::Error::internal(e.to_string()))?;
    v["ok"] = json!(r.success());
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sudohand_shell::FakeShell;

    #[test]
    fn builds_request_and_reports_ok() {
        let sh = FakeShell::new();
        let v = run_with(
            &*sh,
            Cmd::Run {
                sh: true,
                cwd: Some("/tmp".into()),
                envs: vec!["A=1".into()],
                stdin: Some("x".into()),
                timeout_ms: Some(10),
                max_output: 0,
                command: vec!["echo hi".into()],
            },
        )
        .unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["exit_code"], 0);
        let r = &sh.requests()[0];
        assert!(r.via_shell && r.max_output_bytes.is_none());
        assert_eq!(r.env, vec![("A".to_string(), "1".to_string())]);
        assert_eq!(r.stdin.as_deref(), Some(&b"x"[..]));
        let e = run_with(
            &*sh,
            Cmd::Run {
                sh: false,
                cwd: None,
                envs: vec!["bad".into()],
                stdin: None,
                timeout_ms: None,
                max_output: 1,
                command: vec!["ls".into()],
            },
        )
        .unwrap_err();
        assert_eq!(e.code(), "invalid_input");
    }
}
