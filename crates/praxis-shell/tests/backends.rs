#[cfg(unix)]
use praxis_shell::RealShell;
use praxis_shell::{FakeShell, RunRequest, RunResult, ShellBackend};

fn req(program: &str, args: &[&str]) -> RunRequest {
    RunRequest {
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn fake_records_and_answers() {
    let sh = FakeShell::new().with_canned(
        "git",
        RunResult {
            exit_code: Some(128),
            stderr: "fatal: not a git repository".into(),
            ..Default::default()
        },
    );
    let r = sh.run(&req("echo", &["hi"])).unwrap();
    assert!(r.success());
    let r = sh.run(&req("git", &["status"])).unwrap();
    assert_eq!(r.exit_code, Some(128));
    assert!(!r.success());
    let reqs = sh.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].args, vec!["status"]);
}

#[cfg(unix)]
mod real {
    use super::*;

    #[test]
    fn direct_exec_captures_streams_and_exit() {
        let sh = RealShell::new();
        let r = sh.run(&req("printf", &["a\\nb"])).unwrap();
        assert_eq!((r.exit_code, r.stdout.as_str()), (Some(0), "a\nb"));
        assert!(r.success() && !r.timed_out);

        let mut e = req("sh", &["-c", "echo oops >&2; exit 3"]);
        e.cwd = Some(std::env::temp_dir());
        let r = sh.run(&e).unwrap();
        assert_eq!((r.exit_code, r.stderr.trim()), (Some(3), "oops"));
    }

    #[test]
    fn via_shell_env_stdin_and_cwd() {
        let sh = RealShell::new();
        let mut r = req("echo \"$PRAXIS_X:$(pwd):$(cat)\"", &[]);
        r.via_shell = true;
        r.env = vec![("PRAXIS_X".into(), "42".into())];
        r.cwd = Some("/".into());
        r.stdin = Some(b"from-stdin".to_vec());
        let out = sh.run(&r).unwrap();
        assert_eq!(out.stdout.trim(), "42:/:from-stdin");
    }

    #[test]
    fn timeout_kills_and_output_is_capped() {
        let sh = RealShell::new();
        let mut r = req("sleep", &["5"]);
        r.timeout_ms = Some(150);
        let out = sh.run(&r).unwrap();
        assert!(out.timed_out && out.exit_code.is_none() && out.signal.is_some());
        assert!(out.duration_ms < 3000);

        let mut r = req("yes", &[]);
        r.timeout_ms = Some(500);
        r.max_output_bytes = Some(100);
        let out = sh.run(&r).unwrap();
        assert!(out.stdout_truncated && out.stdout.len() == 100);
    }

    #[test]
    fn grandchildren_cannot_outlive_timeout_or_hold_pipes() {
        let sh = RealShell::new();
        // Timeout: the grandchild `sleep` would keep stdout open forever.
        let mut r = req("sleep 30 & wait", &[]);
        r.via_shell = true;
        r.timeout_ms = Some(200);
        let t = std::time::Instant::now();
        let out = sh.run(&r).unwrap();
        assert!(out.timed_out && out.signal.is_some());
        assert!(
            t.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            t.elapsed()
        );

        // No timeout: parent exits, a backgrounded grandchild still holds
        // the pipe; we return after the grace period with what was written.
        let mut r = req("echo before; sleep 20 & exit 0", &[]);
        r.via_shell = true;
        let t = std::time::Instant::now();
        let out = sh.run(&r).unwrap();
        assert_eq!((out.exit_code, out.stdout.trim()), (Some(0), "before"));
        assert!(
            t.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            t.elapsed()
        );

        // A child ignoring SIGTERM still dies (SIGKILL on the group).
        let mut r = req("trap '' TERM; sleep 30", &[]);
        r.via_shell = true;
        r.timeout_ms = Some(200);
        assert!(sh.run(&r).unwrap().timed_out);
    }

    #[test]
    fn stdin_never_read_does_not_block_timeout() {
        let sh = RealShell::new();
        let mut r = req("sleep", &["30"]);
        r.stdin = Some(vec![b'x'; 1 << 20]);
        r.timeout_ms = Some(200);
        let t = std::time::Instant::now();
        assert!(sh.run(&r).unwrap().timed_out);
        assert!(t.elapsed() < std::time::Duration::from_secs(3));
        // …and a reader still gets all of it.
        let mut r = req("wc", &["-c"]);
        r.stdin = Some(vec![b'x'; 1 << 20]);
        assert_eq!(sh.run(&r).unwrap().stdout.trim(), "1048576");
    }

    #[test]
    fn bad_cwd_and_path_override() {
        let sh = RealShell::new();
        let mut r = req("ls", &[]);
        r.cwd = Some("/definitely/not/a/dir".into());
        let e = sh.run(&r).unwrap_err();
        assert_eq!(e.code(), "not_found");
        assert!(e.to_string().starts_with("cwd "), "{e}");

        // `--sh` must not depend on PATH to find the shell.
        let mut r = req("echo hi", &[]);
        r.via_shell = true;
        r.env = vec![("PATH".into(), "/nonexistent".into())];
        assert_eq!(sh.run(&r).unwrap().stdout.trim(), "hi");
    }

    #[test]
    fn spawn_failure_is_not_found() {
        let e = RealShell::new()
            .run(&req("/definitely/not/a/program", &[]))
            .unwrap_err();
        assert_eq!(e.code(), "not_found");
        assert_eq!(
            RealShell::new().run(&req("", &[])).unwrap_err().code(),
            "invalid_input"
        );
    }
}

/// Randomised invariants for the real backend: whatever the mix of delay,
/// output size, exit code, timeout and output cap, the call returns
/// promptly and reports consistently.
#[cfg(unix)]
#[test]
fn real_backend_invariants_under_random_requests() {
    let sh = RealShell::new();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = |n: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for case in 0..48 {
        let delay_ms = rnd(400);
        let bytes = rnd(300_000) as usize;
        let exit = rnd(4) as i32;
        let timeout = if rnd(2) == 0 { None } else { Some(rnd(400)) };
        let cap = match rnd(3) {
            0 => None,
            1 => Some(rnd(1000) as usize),
            _ => Some(1 << 20),
        };
        let script = format!(
            "sleep {}; head -c {bytes} /dev/zero | tr '\\0' x; exit {exit}",
            delay_ms as f64 / 1000.0
        );
        let r = RunRequest {
            program: script.clone(),
            via_shell: true,
            timeout_ms: timeout,
            max_output_bytes: cap,
            ..Default::default()
        };
        let t = std::time::Instant::now();
        let out = sh.run(&r).unwrap();
        let took = t.elapsed().as_millis() as u64;
        let budget = timeout.map_or(delay_ms, |tm| tm.min(delay_ms)) + 2000;
        assert!(
            took <= budget,
            "case {case}: took {took}ms > {budget}ms: {script}"
        );
        // A child killed by the timeout (it was still sleeping) vs. one that
        // finished: the two are mutually exclusive and self-consistent.
        // Only classify when the margin is unambiguous: under parallel test
        // load, shell + head + tr startup can take a few hundred ms.
        const MARGIN: u64 = 500;
        if out.timed_out {
            assert!(
                timeout.is_some_and(|tm| tm <= delay_ms + MARGIN),
                "case {case}: timed out with timeout {timeout:?} vs delay {delay_ms}"
            );
            assert!(out.exit_code.is_none() && out.signal.is_some(), "{out:?}");
        } else {
            assert!(
                timeout.is_none_or(|tm| tm + MARGIN >= delay_ms),
                "case {case}: finished although timeout {timeout:?} < delay {delay_ms}"
            );
            assert_eq!(out.exit_code, Some(exit), "case {case}: {out:?}");
            let expect = cap.map_or(bytes, |c| bytes.min(c));
            assert_eq!(out.stdout.len(), expect, "case {case}: {script}");
            assert_eq!(out.stdout_truncated, cap.is_some_and(|c| bytes > c));
            assert!(out.stdout.bytes().all(|b| b == b'x'));
        }
        assert!(!out.stderr_truncated && out.stderr.is_empty());
    }
}
