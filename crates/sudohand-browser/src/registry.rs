//! Per-instance metadata, validated against Chrome's live browser GUID.
//! This keeps profile/workspace discovery working without automation flags.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::{config, Result};

fn path(port: u16) -> std::path::PathBuf {
    config::base_dir()
        .join("instances")
        .join(format!("{port}.json"))
}

pub(crate) async fn register(
    port: u16,
    pid: u32,
    workspace: &Path,
    profile: &Path,
    argv: &[String],
) -> Result<()> {
    let url =
        crate::cdp::http::ws_debugger_url(config::DEFAULT_DEBUG_HOST, port, Duration::from_secs(5))
            .await?;
    let record = json!({
        "port": port, "pid": pid, "guid": url.rsplit('/').next(),
        "workspace": workspace, "user_data_dir": profile, "argv": argv,
        "identity": null,
    });
    write(port, &record)
}

pub(crate) fn write(port: u16, record: &Value) -> Result<()> {
    let target = path(port);
    std::fs::create_dir_all(target.parent().expect("instance directory"))?;
    let temporary = target.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut file = options.open(&temporary)?;
        file.write_all(serde_json::to_string(record)?.as_bytes())?;
    }
    std::fs::rename(temporary, target)?;
    Ok(())
}

pub(crate) fn lookup(port: u16, websocket: &str) -> Option<Value> {
    let record: Value = serde_json::from_slice(&std::fs::read(path(port)).ok()?).ok()?;
    if record["guid"].as_str() == websocket.rsplit('/').next() {
        Some(record)
    } else {
        remove(port);
        None
    }
}

pub(crate) fn remove(port: u16) {
    let _ = std::fs::remove_file(path(port));
}

pub(crate) fn command_line(port: u16, websocket: &str) -> Option<Vec<String>> {
    let record = lookup(port, websocket)?;
    if let Some(args) = record["argv"].as_array() {
        return Some(
            args.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        );
    }
    // Also accept records written by the reference Python implementation.
    let mut args = Vec::new();
    if let Some(workspace) = record["workspace"].as_str() {
        args.push(format!("--ai-dev-browser-workspace={workspace}"));
    }
    if let Some(profile) = record["user_data_dir"].as_str() {
        args.push(format!("--user-data-dir={profile}"));
    }
    Some(args)
}
