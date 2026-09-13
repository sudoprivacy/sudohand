//! Exercise the actual CLI main-thread stack, not a test runner's worker stack.
#[test]
fn offline_browser_command_returns_an_error_without_stack_overflow() {
    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_suh"))
        .args([
            "browser",
            "cookies_extract_offline",
            "--domain",
            "fixture.test",
            "--user-data-dir",
        ])
        .arg(directory.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    let error: serde_json::Value = serde_json::from_str(&stderr).unwrap();
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Could not find chrome cookie database"));
    assert!(output.stdout.is_empty());
}
