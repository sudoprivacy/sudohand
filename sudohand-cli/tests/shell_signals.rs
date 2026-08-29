//! `sudohand shell run` must not leave its child behind when sudohand itself is
//! interrupted (Ctrl-C / SIGTERM) — the child runs in its own process group,
//! so the CLI forwards those signals to the group.

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::Duration;

fn child_alive(marker: &str) -> bool {
    let out = Command::new("pgrep").args(["-f", marker]).output().unwrap();
    !out.stdout.is_empty()
}

fn check(sig: &str) {
    let marker = format!("sudohand-signal-test-{}-{sig}", std::process::id());
    let mut sudohand = Command::new(env!("CARGO_BIN_EXE_sudohand"))
        .args([
            "shell",
            "run",
            "--sh",
            "--",
            &format!("sleep 30 # {marker}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(child_alive(&marker), "child did not start");
    assert!(Command::new("kill")
        .args([&format!("-{sig}"), &sudohand.id().to_string()])
        .status()
        .unwrap()
        .success());
    let status = sudohand.wait().unwrap();
    assert!(!status.success());
    std::thread::sleep(Duration::from_millis(300));
    let alive = child_alive(&marker);
    let _ = Command::new("pkill").args(["-f", &marker]).status();
    assert!(!alive, "child survived sudohand being killed with SIG{sig}");
}

#[test]
fn sigint_and_sigterm_take_the_child_down() {
    check("INT");
    check("TERM");
}
