//! `suh <name> …` execs `suh-<name>` from the extension search path,
//! passing the remaining args, `SUH_BIN` / `SUH_EXT_NAME`, and the exit code.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn suh() -> Command {
    Command::new(env!("CARGO_BIN_EXE_suh"))
}

fn fixture(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("suh-ext-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("hello")).unwrap();
    let script = d.join("hello").join("suh-hello");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '{\"args\":\"%s\",\"name\":\"%s\",\"bin\":\"%s\"}\\n' \"$*\" \"$SUH_EXT_NAME\" \"$SUH_BIN\"\nexit 3\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

#[test]
fn dispatches_to_extension() {
    let d = fixture("run");
    let out = suh()
        .env("SUH_EXT_PATH", &d)
        .args(["hello", "world", "--flag"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["args"], "world --flag");
    assert_eq!(v["name"], "hello");
    assert_eq!(v["bin"], env!("CARGO_BIN_EXE_suh"));
    assert_eq!(out.status.code(), Some(3), "exit code propagates");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn ext_list_and_which() {
    let d = fixture("list");
    let out = suh()
        .env("SUH_EXT_PATH", &d)
        .env("PATH", "") // hide anything installed on the machine
        .env("HOME", &d)
        .args(["ext", "list"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = v["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    // `suh-wx` sits next to the test binary's `suh` (sibling) when built in
    // the same workspace; only assert what this fixture guarantees.
    assert!(names.contains(&"hello"), "{names:?}");

    let out = suh()
        .env("SUH_EXT_PATH", &d)
        .args(["ext", "which", "hello"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["source"], "ext_path");
    assert!(v["path"].as_str().unwrap().ends_with("hello/suh-hello"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn unknown_command_is_not_found() {
    let out = suh()
        .env("SUH_EXT_PATH", "")
        .env("PATH", "")
        .env("HOME", "/nonexistent")
        .args(["definitely-not-installed", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(v["error"]["kind"], "not_found");
    assert!(out.stdout.is_empty());
}
