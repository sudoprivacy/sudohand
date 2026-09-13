//! Browser lifecycle: `browser_start` / `browser_stop` / `browser_list`.
//! Port of `core/browser.py`.

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
    /// Omit automation markers; None defaults to true.
    pub stealth: Option<bool>,
    /// Timezone override persisted across CLI sessions.
    pub timezone: Option<String>,
    /// Geolocation as latitude,longitude.
    pub geo: Option<String>,
    /// Explicit locale override.
    pub locale: Option<String>,
    /// Derive timezone and geolocation through the browser proxy.
    pub match_proxy: Option<bool>,
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
    launch.stealth = opts.stealth.unwrap_or(true);
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
        let expect_page = !opts
            .extra_args
            .iter()
            .any(|arg| arg == "--no-startup-window");
        if is_port_in_use(port) && devtools_ready(port, expect_page).await {
            listening = true;
            break;
        }
        if let Ok(Some(_status)) = launched.child.try_wait() {
            let mut stderr = launched
                .stderr
                .as_ref()
                .map_or_else(String::new, crate::launch_stderr::LaunchStderr::snapshot);
            if stderr.trim().is_empty() {
                stderr = "Chrome exited silently. Possible causes:\n  - Another Chrome is using this profile\n  - Profile directory is corrupted\n  - Insufficient permissions".to_string();
            }
            return Ok(json!({"error": format!("Chrome process exited unexpectedly: {stderr}")}));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !listening {
        let _ = kill_process_tree(pid);
        let stderr = launched
            .stderr
            .as_ref()
            .map_or_else(String::new, crate::launch_stderr::LaunchStderr::snapshot);
        let diagnostic = if stderr.trim().is_empty() {
            String::new()
        } else {
            format!("\nRecent Chrome stderr:\n{stderr}")
        };
        return Ok(json!({
            "error": format!(
                "Chrome started (PID {pid}) but DevTools/initial page on port {port} was not ready after {timeout}s — process killed to release profile lockfile. Retry with startup_timeout=<larger> if your environment is slow.{diagnostic}"
            ),
            "pid": pid,
        }));
    }
    // Registry metadata is best effort: a read-only home must not kill a launch.
    let _ = crate::registry::register(
        port,
        pid,
        &launch.workspace,
        &launched.user_data_dir,
        &crate::chrome::build_args(&launch, &launched.user_data_dir),
    )
    .await;
    // Detach: the Child handle must not reap/kill Chrome when we exit.
    drop(launched.stderr.take());
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
    crate::identity::configure(port, opts, &mut out).await;
    Ok(Value::Object(out))
}

async fn devtools_ready(port: u16, expect_page: bool) -> bool {
    if crate::cdp::http::ws_debugger_url(DEFAULT_DEBUG_HOST, port, Duration::from_secs(2))
        .await
        .is_err()
    {
        return false;
    }
    if !expect_page {
        return true;
    }
    // Chrome may expose DevTools before publishing its startup tab. Returning
    // then makes get_active_tab create an extra blank tab, especially on Windows.
    crate::cdp::http::get_json(
        DEFAULT_DEBUG_HOST,
        port,
        "/json/list",
        Duration::from_secs(2),
    )
    .await
    .is_ok_and(|targets| {
        targets
            .as_array()
            .is_some_and(|items| items.iter().any(|target| target["type"] == "page"))
    })
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

/// Stop one Chrome (by port) or all GUID-validated registered Chromes (`stop_all`).
/// `Browser.close` first (flushes the profile), force-kill as fallback.
pub async fn browser_stop(port: Option<u16>, stop_all: bool) -> Result<Value> {
    if port.is_none() && !stop_all {
        return Ok(json!({"error": "Please specify port or stop_all"}));
    }
    let mut stopped = Vec::new();
    if stop_all {
        for c in find_debug_chromes(DEFAULT_PORT_RANGE).await {
            if let Some(pid) = c.pid {
                let Ok(websocket) = crate::cdp::http::ws_debugger_url(
                    DEFAULT_DEBUG_HOST,
                    c.port,
                    Duration::from_secs(2),
                )
                .await
                else {
                    continue;
                };
                if crate::registry::lookup(c.port, &websocket).is_some() {
                    stopped.push(graceful_stop(c.port, pid, 5.0).await);
                }
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
    crate::cleanup::list_chromes(all_workspaces).await
}

/// Inspect an existing browser connection; no new Chrome is launched.
pub async fn browser_connect(transport: Option<&str>, port: Option<u16>) -> Result<Value> {
    let transport = transport
        .map(str::to_owned)
        .or_else(|| std::env::var("AI_DEV_BROWSER_TRANSPORT").ok())
        .unwrap_or_else(|| "cdp".into());
    if transport == "extension" {
        let status = crate::bridge::status(crate::bridge::PORT).await;
        if status
            .as_ref()
            .is_some_and(|state| state["extension_connected"] == true)
        {
            let browser = BrowserClient::connect_extension(crate::bridge::PORT).await?;
            let tabs: Vec<_> = browser
                .page_targets()
                .into_iter()
                .map(|target| &target.url)
                .collect();
            return Ok(json!({"transport": "extension", "connected": true,
                "account": status.as_ref().map(|state| &state["account"]),
                "tab_count": tabs.len(), "tabs": tabs, "bridge_port": crate::bridge::PORT}));
        }
        let directory = crate::extension::extension_dir()?;
        return Ok(json!({"transport": "extension", "connected": false,
            "bridge_running": status.is_some(), "retryable": true,
            "extension_dir": directory,
            "setup_instructions": format!("Open chrome://extensions in the profile you want to control. Enable Developer mode, click Load unpacked, and select {}. Keep Chrome and the extension running, then retry browser_connect --transport extension.", directory.display())}));
    }
    if transport != "cdp" {
        return Err(crate::Error::Invalid(format!(
            "Unknown browser transport: {transport}"
        )));
    }
    let port = crate::connection::resolve_port(port).await;
    match BrowserClient::connect(DEFAULT_DEBUG_HOST, port).await {
        Ok(browser) => {
            let tabs: Vec<_> = browser
                .page_targets()
                .into_iter()
                .map(|target| &target.url)
                .collect();
            Ok(
                json!({"transport": "cdp", "connected": true, "port": browser.port,
                "tab_count": tabs.len(), "tabs": tabs}),
            )
        }
        Err(error) => Ok(
            json!({"transport": "cdp", "connected": false, "retryable": false, "error": error.to_string()}),
        ),
    }
}
