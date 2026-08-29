use praxis_shell::{FakeShell, RealShell, RunRequest, RunResult, ShellBackend};

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
