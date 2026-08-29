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
    assert_eq!(out.status.code(), Some(4)); // not_found
    let v: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(v["error"]["kind"], "not_found");
    assert!(out.stdout.is_empty());
}

/// A shell extension that honours the whole contract.
fn conformant_script(dir: &std::path::Path, name: &str, version: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(format!("suh-{name}"));
    std::fs::write(
        &p,
        format!(
            r#"#!/bin/sh
case "$1" in
  --manifest) echo '{{"schema":1,"name":"{name}","description":"t","version":"{version}","platforms":["macos","linux"],"commands":[]}}';;
  --help) echo "usage: suh {name}";;
  workflows) echo '{{"workflows":[]}}';;
  ping) echo '{{"pong":"{version}"}}';;
  *) echo '{{"error":{{"kind":"invalid_input","message":"unknown command"}}}}' >&2; exit 1;;
esac
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn home(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("suh-home-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "not json. stdout: {} stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn install_from_file_then_update_and_uninstall() {
    let h = home("file");
    let src = h.join("src");
    let script = conformant_script(&src, "pinger", "1.0.0");
    let suh_in = |args: &[&str]| {
        suh()
            .env("HOME", &h)
            .env("SUH_EXT_PATH", "")
            .env("PATH", std::env::var("PATH").unwrap())
            .args(args)
            .output()
            .unwrap()
    };

    let out = suh_in(&["ext", "install", script.to_str().unwrap()]);
    let v = json(&out);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(v["installed"], "pinger");
    assert_eq!(v["version"], "1.0.0");
    assert_eq!(v["source"]["kind"], "file");
    let installed = h.join(".suh/extensions/pinger/suh-pinger");
    assert!(installed.is_file());
    assert!(h.join(".suh/extensions/pinger/install.json").is_file());

    // dispatch now finds it via ~/.suh/extensions
    let out = suh_in(&["pinger", "ping"]);
    assert_eq!(json(&out)["pong"], "1.0.0");

    // list shows the recorded version
    let v = json(&suh_in(&["ext", "list"]));
    let e = v["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "pinger")
        .unwrap();
    assert_eq!(e["version"], "1.0.0");
    assert_eq!(e["source"], "home");

    // the source file changes; update re-copies it
    conformant_script(&src, "pinger", "1.1.0");
    let v = json(&suh_in(&["ext", "update", "pinger"]));
    assert_eq!(v["version"], "1.1.0");
    assert_eq!(v["previous_version"], "1.0.0");
    assert_eq!(json(&suh_in(&["pinger", "ping"]))["pong"], "1.1.0");

    // uninstall removes the managed dir
    let v = json(&suh_in(&["ext", "uninstall", "pinger"]));
    assert_eq!(v["uninstalled"], "pinger");
    assert!(!installed.exists());
    let out = suh_in(&["ext", "uninstall", "pinger"]);
    assert_eq!(out.status.code(), Some(4)); // not_found
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stderr).unwrap()["error"]["kind"],
        "not_found"
    );
    let _ = std::fs::remove_dir_all(h);
}

#[test]
fn install_refuses_nonconformant_unless_forced() {
    let h = home("bad");
    let bad = h.join("suh-bad");
    // has a manifest but no error envelope / help
    std::fs::write(
        &bad,
        "#!/bin/sh\n[ \"$1\" = --manifest ] && echo '{\"schema\":1,\"name\":\"bad\",\"description\":\"\",\"version\":\"0\",\"platforms\":[],\"commands\":[]}' && exit 0\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |extra: &[&str]| {
        let mut args = vec!["ext", "install", bad.to_str().unwrap()];
        args.extend_from_slice(extra);
        suh()
            .env("HOME", &h)
            .env("SUH_EXT_PATH", "")
            .args(args)
            .output()
            .unwrap()
    };
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(2)); // invalid_input
    let err = serde_json::from_slice::<serde_json::Value>(&out.stderr).unwrap();
    assert_eq!(err["error"]["kind"], "invalid_input");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--force"));
    assert!(!h.join(".suh/extensions/bad").exists());

    let out = run(&["--force"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(h.join(".suh/extensions/bad/suh-bad").is_file());
    let _ = std::fs::remove_dir_all(h);
}

/// A dependency-free cargo project that is a conformant extension.
#[test]
fn install_from_cargo_project() {
    let h = home("cargo");
    let proj = h.join("suh-tiny");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname = \"suh-tiny\"\nversion = \"0.3.0\"\nedition = \"2021\"\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/main.rs"),
        r##"fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let out = match a.first().map(String::as_str) {
        Some("--manifest") => r#"{"schema":1,"name":"tiny","description":"t","version":"0.3.0","platforms":["macos","linux"],"commands":[]}"#,
        Some("--help") => "usage: suh tiny",
        Some("workflows") => r#"{"workflows":[]}"#,
        Some("ping") => r#"{"pong":true}"#,
        _ => {
            eprintln!("{}", r#"{"error":{"kind":"invalid_input","message":"unknown command"}}"#);
            std::process::exit(1)
        }
    };
    println!("{}", out);
}
"##,
    )
    .unwrap();
    let out = suh()
        .env("HOME", &h)
        .env("SUH_EXT_PATH", "")
        .args(["ext", "install", proj.to_str().unwrap()])
        .output()
        .unwrap();
    let v = json(&out);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(v["installed"], "tiny");
    assert_eq!(v["version"], "0.3.0");
    assert_eq!(v["source"]["kind"], "project");
    let out = suh()
        .env("HOME", &h)
        .env("SUH_EXT_PATH", "")
        .args(["tiny", "ping"])
        .output()
        .unwrap();
    assert_eq!(json(&out)["pong"], true);
    let _ = std::fs::remove_dir_all(h);
}
