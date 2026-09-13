//! Process inventory and explicitly scoped cleanup of managed Chrome orphans.
use crate::{config, Error, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};
use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};

/// Required scope for orphan cleanup; no blanket default.
#[derive(Debug, Clone, Copy)]
pub enum CleanupScope {
    Temp,
    Profile,
    Workspace,
}

impl CleanupScope {
    fn name(self) -> &'static str {
        match self {
            Self::Temp => "temp",
            Self::Profile => "profile",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Clone)]
struct Chrome {
    pid: u32,
    started: u64,
    port: Option<u16>,
    directory: Option<PathBuf>,
    profile: String,
}

fn processes() -> Vec<Chrome> {
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(
            ProcessRefreshKind::nothing()
                .without_tasks()
                .with_cmd(UpdateKind::Always),
        ),
    );
    let mut chromes: Vec<Chrome> = system
        .processes()
        .values()
        .filter_map(|process| {
            let name = process.name().to_string_lossy().to_lowercase();
            if !name.contains("chrome") && !name.contains("chromium") && !name.contains("msedge") {
                return None;
            }
            let args: Vec<_> = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            if args.iter().any(|arg| arg.starts_with("--type=")) {
                return None;
            }
            let value = |flag: &str| {
                args.iter()
                    .find_map(|arg| arg.strip_prefix(&format!("{flag}=")).map(str::to_owned))
            };
            Some(Chrome {
                pid: process.pid().as_u32(),
                started: process.start_time(),
                port: value("--remote-debugging-port").and_then(|s| s.parse().ok()),
                directory: value("--user-data-dir").map(PathBuf::from),
                profile: value("--profile-directory").unwrap_or_else(|| "Default".into()),
            })
        })
        .collect();
    chromes.sort_by_key(|chrome| chrome.pid);
    chromes
}

fn normalized(path: &Path) -> PathBuf {
    PathBuf::from(config::normalize_workspace(path))
}

fn managed(path: &Path) -> bool {
    let path = normalized(path);
    path.starts_with(normalized(&config::profile_root())) || is_temp(&path)
}

fn is_temp(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .starts_with(config::DEFAULT_PROFILE_PREFIX)
    }) && path.starts_with(normalized(&std::env::temp_dir()))
}

fn workspace(path: &Path) -> Option<String> {
    normalized(path)
        .strip_prefix(normalized(&config::profile_root()))
        .ok()?
        .components()
        .next()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
}

async fn live(chrome: &Chrome) -> bool {
    let Some(port) = chrome.port else {
        return false;
    };
    crate::cdp::http::ws_debugger_url(config::DEFAULT_DEBUG_HOST, port, Duration::from_secs(2))
        .await
        .is_ok()
}

/// Inventory every visible Chrome main process, including non-debug browsers.
pub async fn list_chromes(all_workspaces: bool) -> Result<Value> {
    let current = config::workspace_slug(&config::current_workspace());
    let mut browsers = Vec::new();
    for chrome in processes() {
        let owned = chrome.directory.as_deref().is_some_and(managed);
        let workspace = chrome.directory.as_deref().and_then(workspace);
        if !all_workspaces && owned && workspace.as_ref().is_some_and(|ws| ws != &current) {
            continue;
        }
        let origin = if !owned {
            "external"
        } else if live(&chrome).await {
            "adb"
        } else {
            "adb-orphan"
        };
        browsers.push(
            json!({"pid": chrome.pid, "port": chrome.port, "origin": origin,
            "user_data_dir": chrome.directory, "profile": chrome.profile, "workspace": workspace}),
        );
    }
    let order = |v: &Value| match v["origin"].as_str() {
        Some("adb") => 0,
        Some("adb-orphan") => 1,
        _ => 2,
    };
    browsers.sort_by_key(|v| (order(v), v["pid"].as_u64()));
    let mut counts = serde_json::Map::new();
    for browser in &browsers {
        let key = browser["origin"].as_str().expect("origin");
        let next = counts.get(key).and_then(Value::as_u64).unwrap_or(0) + 1;
        counts.insert(key.into(), json!(next));
    }
    Ok(
        json!({"count": browsers.len(), "browsers": browsers, "by_origin": counts, "process_inventory": "sysinfo"}),
    )
}

/// Kill only scoped managed orphans. Revalidate PID identity and port immediately
/// before killing; a listening port is conservatively protected even if CDP fails.
pub async fn browser_cleanup(
    scope: CleanupScope,
    profile: Option<&str>,
    dry_run: bool,
) -> Result<Value> {
    let target = match scope {
        CleanupScope::Profile => {
            let name = profile
                .filter(|name| !name.is_empty())
                .ok_or_else(|| Error::Invalid("scope='profile' requires a profile name".into()))?;
            let path = normalized(&config::workspace_profile_dir(name, None));
            if !path.starts_with(normalized(&config::profile_root())) {
                return Err(Error::Invalid(
                    "profile must be inside the managed profile directory".into(),
                ));
            }
            Some(path)
        }
        CleanupScope::Workspace => Some(normalized(
            &config::profile_root().join(config::workspace_slug(&config::current_workspace())),
        )),
        CleanupScope::Temp => None,
    };
    let eligible = |chrome: &Chrome| {
        let Some(path) = chrome.directory.as_deref().map(normalized) else {
            return false;
        };
        managed(&path)
            && match scope {
                CleanupScope::Temp => is_temp(&path),
                CleanupScope::Profile => Some(&path) == target.as_ref(),
                CleanupScope::Workspace => {
                    path.starts_with(target.as_ref().expect("workspace target"))
                }
            }
    };
    let mut candidates = Vec::new();
    for chrome in processes().into_iter().filter(&eligible) {
        if chrome.port.is_some_and(crate::port::is_port_in_use) || live(&chrome).await {
            continue;
        }
        candidates.push(chrome);
    }
    if dry_run {
        let preview: Vec<_> = candidates
            .iter()
            .map(|chrome| json!({"pid": chrome.pid, "user_data_dir": chrome.directory}))
            .collect();
        return Ok(
            json!({"scope": scope.name(), "dry_run": true, "count": preview.len(), "would_kill": preview}),
        );
    }
    let mut killed = Vec::new();
    let mut directories = BTreeSet::new();
    for candidate in candidates {
        // A PID can have been reused between inventory and action.
        let verified = processes().into_iter().find(|now| {
            now.pid == candidate.pid
                && now.started == candidate.started
                && now.directory == candidate.directory
                && eligible(now)
        });
        if let Some(now) = verified {
            if now.port.is_some_and(crate::port::is_port_in_use) {
                continue;
            }
            if crate::port::kill_process_tree(now.pid) {
                killed.push(now.pid);
                if let Some(path) = now.directory {
                    directories.insert(path);
                }
            }
        }
    }
    Ok(
        json!({"scope": scope.name(), "dry_run": false, "count": killed.len(), "killed": killed, "profile_dirs": directories}),
    )
}
