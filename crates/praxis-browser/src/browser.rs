//! Browser lifecycle: `browser_start` / `browser_stop` / `browser_list`.
//! Port of `core/browser.py`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::chrome::{launch_chrome, Headless, LaunchOptions};
use crate::config::{
    self, workspace_profile_dir, DEFAULT_DEBUG_HOST, DEFAULT_PORT_RANGE, HEADLESS_ENV,
};
use crate::connection::BrowserClient;
use crate::port::{
    cleanup_temp_profile, find_debug_chromes, find_workspace_chromes, get_available_port,
    is_port_in_use, kill_process_tree, pid_on_port_async, query_chrome_cmdline, FoundChrome,
};
use crate::Result;

/// Reuse strategy for a named profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reuse {
    /// Always start a new Chrome.
    None,
    /// Reuse an idle same-profile / same-workspace Chrome (default).
    #[default]
    Any,
}

/// Options for [`browser_start`].
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// Debug port (auto-assigned if `None`).
    pub port: Option<u16>,
    /// Headless mode; `None` → `AI_DEV_BROWSER_HEADLESS`.
    pub headless: Option<Headless>,
    /// Initial URL.
    pub url: Option<String>,
    /// Named persistent profile.
    pub profile: Option<String>,
    /// Force a temp profile.
    pub temp: bool,
    /// Reuse strategy.
    pub reuse: Reuse,
    /// Seconds to wait for the debug port (default 30).
    pub startup_timeout: Option<f64>,
    /// Extra Chrome flags.
    pub extra_args: Vec<String>,
    /// Override/remove default flags.
    pub override_default_args: Vec<(String, Option<String>)>,
    /// Route Chrome's stderr to null.
    pub silent_stderr: bool,
}

async fn find_chrome_using_profile(profile_dir: &Path) -> Option<FoundChrome> {
    let want = profile_dir.to_string_lossy().into_owned();
    for c in find_debug_chromes(DEFAULT_PORT_RANGE).await {
        if let Some(cmd) = query_chrome_cmdline(c.port, Duration::from_secs(2)).await {
            if cmd.iter().any(|a| a.contains(&want)) {
                return Some(c);
            }
        }
    }
    None
}

async fn find_reusable(profile: Option<&str>) -> Option<u16> {
    match profile {
        Some(p) => find_chrome_using_profile(&workspace_profile_dir(p, None))
            .await
            .map(|c| c.port),
        None => find_workspace_chromes(&config::current_workspace(), DEFAULT_PORT_RANGE)
            .await
            .first()
            .map(|c| c.port),
    }
}

fn env_headless() -> Headless {
    Headless::parse(&std::env::var(HEADLESS_ENV).unwrap_or_default())
}

fn headless_json(h: Headless) -> Value {
    match h {
        Headless::Off => json!(false),
        Headless::New => json!("new"),
        Headless::Old => json!("old"),
    }
}

/// Start a browser — isolated (temp profile, no reuse) unless a `profile`
/// is named. Returns `{port, pid, headless, url, profile, reused, message}`
/// or `{error}`.
pub async fn browser_start(opts: &StartOptions) -> Result<Value> {
    let temp = opts.temp || opts.profile.is_none();

    if opts.reuse != Reuse::None && !temp {
        if let Some(port) = find_reusable(opts.profile.as_deref()).await {
            let pid = pid_on_port_async(port).await;
            return Ok(json!({
                "port": port,
                "pid": pid,
                "profile": opts.profile.clone().unwrap_or_else(|| "default".into()),
                "reused": true,
                "message": format!("Reusing existing Chrome on port {port}"),
            }));
        }
    }

    let mut env_port_warning: Option<String> = None;
    let port = match opts.port {
        Some(p) => {
            if is_port_in_use(p) {
                let pid = pid_on_port_async(p).await;
                return Ok(json!({"error": format!(
                    "Port {p} is already in use (PID: {}). Use a different port or stop the existing process.",
                    pid.map_or("None".to_string(), |x| x.to_string())
                )}));
            }
            p
        }
        // `AI_DEV_BROWSER_PORT` is honoured here too, so `export` + start +
        // connect stay on one port. If it is taken we fall back to
        // allocation and say so in the result.
        None => match config::env_port() {
            Some(p) if !is_port_in_use(p) => p,
            Some(p) => {
                env_port_warning = Some(format!(
                    "AI_DEV_BROWSER_PORT={p} is already in use; started on a different port — \
                     later commands reading the env var will connect to {p}, not this Chrome"
                ));
                get_available_port(DEFAULT_PORT_RANGE, &[p])?
            }
            None => get_available_port(DEFAULT_PORT_RANGE, &[])?,
        },
    };

    let (user_data_dir, profile_name): (Option<PathBuf>, String) = if temp {
        (None, "(temp)".to_string())
    } else {
        let name = opts.profile.clone().unwrap_or_else(|| "default".into());
        let dir = workspace_profile_dir(&name, None);
        std::fs::create_dir_all(&dir)?;
        if let Some(existing) = find_chrome_using_profile(&dir).await {
            return Ok(json!({
                "port": existing.port,
                "pid": existing.pid,
                "profile": name,
                "reused": true,
                "message": format!("Profile '{name}' already in use. Reusing Chrome on port {}.", existing.port),
            }));
        }
        (Some(dir), name)
    };

    let start_url = opts.url.clone().unwrap_or_else(|| "about:blank".into());
    let headless = opts.headless.unwrap_or_else(env_headless);
    let mut launch = LaunchOptions::new(port);
    launch.headless = headless;
    launch.user_data_dir = user_data_dir;
    launch.extra_args.clone_from(&opts.extra_args);
    launch
        .override_default_args
        .clone_from(&opts.override_default_args);
    launch.silent_stderr = opts.silent_stderr;
    launch.start_url.clone_from(&start_url);
    launch.window_size = config::resolve_viewport()?;
    let mut launched = launch_chrome(&launch)?;
    let pid = launched.child.id();

    let timeout = opts.startup_timeout.unwrap_or(30.0);
    let start = std::time::Instant::now();
    let mut listening = false;
    while start.elapsed().as_secs_f64() < timeout {
        // Readiness = the DevTools HTTP endpoint answers, not merely a bound
        // socket: right after bind, a Chrome competing with other launches
        // can take seconds before /json/version responds, and a caller that
        // connects on our return must not race that.
        if is_port_in_use(port) && devtools_ready(port).await {
            listening = true;
            break;
        }
        if let Ok(Some(_status)) = launched.child.try_wait() {
            let mut stderr = String::new();
            if let Some(mut s) = launched.child.stderr.take() {
                let _ = s.read_to_string(&mut stderr);
            }
            if stderr.trim().is_empty() {
                stderr = "Chrome exited silently. Possible causes:\n  - Another Chrome is using this profile\n  - Profile directory is corrupted\n  - Insufficient permissions".to_string();
            }
            return Ok(json!({"error": format!("Chrome process exited unexpectedly: {stderr}")}));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !listening {
        let _ = kill_process_tree(pid);
        return Ok(json!({
            "error": format!(
                "Chrome started (PID {pid}) but port {port} not listening after {timeout}s — process killed to release profile lockfile. Retry with startup_timeout=<larger> if your environment is slow."
            ),
            "pid": pid,
        }));
    }
    // Detach: the Child handle must not reap/kill Chrome when we exit.
    drop(launched.child.stderr.take());
    let mut out = serde_json::Map::new();
    out.insert("port".into(), json!(port));
    out.insert("pid".into(), json!(pid));
    out.insert("headless".into(), headless_json(headless));
    out.insert("url".into(), json!(start_url));
    out.insert("profile".into(), json!(profile_name));
    out.insert("reused".into(), json!(false));
    out.insert(
        "message".into(),
        json!(format!("Browser started on port {port}")),
    );
    if let Some(w) = env_port_warning {
        out.insert("warning".into(), json!(w));
    }
    Ok(Value::Object(out))
}

async fn devtools_ready(port: u16) -> bool {
    crate::cdp::http::ws_debugger_url(DEFAULT_DEBUG_HOST, port, Duration::from_secs(2))
        .await
        .is_ok()
}

async fn graceful_stop(port: u16, pid: u32, timeout: f64) -> Value {
    let sent = match BrowserClient::connect(DEFAULT_DEBUG_HOST, port).await {
        Ok(b) => b.close_browser().await.is_ok(),
        Err(_) => false,
    };
    if sent {
        let start = std::time::Instant::now();
        while start.elapsed().as_secs_f64() < timeout {
            if !is_port_in_use(port) {
                let _ = cleanup_temp_profile(port);
                return json!({"port": port, "pid": pid, "method": "graceful"});
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    let _ = kill_process_tree(pid);
    let _ = cleanup_temp_profile(port);
    json!({"port": port, "pid": pid, "method": "force"})
}

/// Stop one Chrome (by port) or every debugging Chrome (`stop_all`).
/// `Browser.close` first (flushes the profile), force-kill as fallback.
pub async fn browser_stop(port: Option<u16>, stop_all: bool) -> Result<Value> {
    if port.is_none() && !stop_all {
        return Ok(json!({"error": "Please specify port or stop_all"}));
    }
    let mut stopped = Vec::new();
    if stop_all {
        for c in find_debug_chromes(DEFAULT_PORT_RANGE).await {
            if let Some(pid) = c.pid {
                stopped.push(graceful_stop(c.port, pid, 5.0).await);
            }
        }
    } else if let Some(p) = port {
        if let Some(pid) = pid_on_port_async(p).await {
            stopped.push(graceful_stop(p, pid, 5.0).await);
        }
    }
    Ok(json!({"stopped": true, "count": stopped.len(), "browsers": stopped}))
}

/// List debugging Chromes — this workspace's by default, or all.
pub async fn browser_list(all_workspaces: bool) -> Result<Value> {
    let browsers: Vec<Value> = if all_workspaces {
        find_debug_chromes(DEFAULT_PORT_RANGE)
            .await
            .into_iter()
            .map(|c| {
                let mut m = serde_json::Map::new();
                m.insert("port".into(), json!(c.port));
                m.insert("pid".into(), json!(c.pid));
                if let Some(w) = c.workspace {
                    m.insert("workspace".into(), json!(w));
                }
                Value::Object(m)
            })
            .collect()
    } else {
        find_workspace_chromes(&config::current_workspace(), DEFAULT_PORT_RANGE)
            .await
            .into_iter()
            .map(|c| json!({"port": c.port, "pid": c.pid}))
            .collect()
    };
    Ok(json!({"browsers": browsers, "count": browsers.len()}))
}
