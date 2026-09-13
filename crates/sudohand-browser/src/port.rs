//! Port management and Chrome discovery. Mirrors `core/port.py` +
//! `core/process.py`, with the scan made concurrent.
//!
//! `AI_DEV_BROWSER_PORT` short-circuits *every* scan (see
//! [`crate::connection::resolve_port`]) — the scan is the slow path under
//! gVisor, so it must never run when the caller already told us the port.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::browser::GetBrowserCommandLineParams;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

use crate::cdp::{http, Connection};
use crate::config::{
    normalize_workspace, DEFAULT_DEBUG_HOST, DEFAULT_PORT_RANGE, DEFAULT_PROFILE_PREFIX,
    WORKSPACE_FLAG,
};
use crate::Result;

/// Per-listener CDP probe budget (HTTP `/json/version` + WS + one command).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// A discovered debugging Chrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundChrome {
    /// Debug port.
    pub port: u16,
    /// Listening PID, if resolvable.
    pub pid: Option<u32>,
    /// `--ai-dev-browser-workspace` value, if present.
    pub workspace: Option<String>,
}

fn trace(msg: impl FnOnce() -> String) {
    if std::env::var_os("ADB_TRACE").is_some() {
        eprintln!("[adb trace] {}", msg());
    }
}

fn addr(host: &str, port: u16) -> SocketAddr {
    let ip = host.parse().unwrap_or(std::net::Ipv4Addr::LOCALHOST.into());
    SocketAddr::new(ip, port)
}

/// True if `port` can be bound on `host` (not reserved by the OS).
#[must_use]
pub fn is_port_bindable(host: &str, port: u16) -> bool {
    TcpListener::bind(addr(host, port)).is_ok()
}

/// Is something listening on `port`? Bind-first for speed, then a short
/// connect on IPv4 and IPv6.
#[must_use]
pub fn is_port_in_use(port: u16) -> bool {
    if TcpListener::bind(addr(DEFAULT_DEBUG_HOST, port)).is_ok() {
        return false;
    }
    let t = Duration::from_millis(100);
    if std::net::TcpStream::connect_timeout(&addr(DEFAULT_DEBUG_HOST, port), t).is_ok() {
        return true;
    }
    std::net::TcpStream::connect_timeout(
        &SocketAddr::new(std::net::Ipv6Addr::LOCALHOST.into(), port),
        t,
    )
    .is_ok()
}

async fn fast_listening_check(port: u16) -> bool {
    if TcpListener::bind(addr(DEFAULT_DEBUG_HOST, port)).is_ok() {
        return false;
    }
    // Bind failed, so something holds the port; give a busy Chrome (e.g. one
    // still starting up under load) a real chance to accept.
    matches!(
        tokio::time::timeout(
            Duration::from_millis(100),
            TcpStream::connect(addr(DEFAULT_DEBUG_HOST, port))
        )
        .await,
        Ok(Ok(_))
    )
}

/// Read GUID-validated launch metadata, falling back to CDP command-line readback.
/// `None` if the port is not a Chrome debug endpoint.
pub async fn query_chrome_cmdline(port: u16, timeout: Duration) -> Option<Vec<String>> {
    let ws = http::ws_debugger_url(DEFAULT_DEBUG_HOST, port, timeout)
        .await
        .ok()?;
    if let Some(args) = crate::registry::command_line(port, &ws) {
        return Some(args);
    }
    let conn = tokio::time::timeout(timeout, Connection::connect(&ws))
        .await
        .ok()?
        .ok()?;
    let out = conn
        .send_with(GetBrowserCommandLineParams::default(), timeout, None)
        .await
        .ok()?;
    Some(out.arguments)
}

/// Extract the `--ai-dev-browser-workspace=` value.
#[must_use]
pub fn extract_workspace(cmdline: &[String]) -> Option<String> {
    let prefix = format!("{WORKSPACE_FLAG}=");
    cmdline.iter().find_map(|a| {
        a.strip_prefix(&prefix)
            .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
    })
}

/// Scan `[start, end)` for Chrome debug instances, concurrently.
pub async fn scan_ports_for_chrome(range: (u16, u16)) -> Vec<FoundChrome> {
    let listen_sem = Arc::new(Semaphore::new(256));
    let mut handles = Vec::new();
    for port in range.0..range.1 {
        let sem = Arc::clone(&listen_sem);
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok()?;
            fast_listening_check(port).await.then_some(port)
        }));
    }
    let mut listening = Vec::new();
    for h in handles {
        if let Ok(Some(p)) = h.await {
            listening.push(p);
        }
    }
    trace(|| format!("scan {range:?}: listening={listening:?}"));
    if listening.is_empty() {
        return Vec::new();
    }
    let probe_sem = Arc::new(Semaphore::new(16));
    let mut handles = Vec::new();
    for port in listening {
        let sem = Arc::clone(&probe_sem);
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok()?;
            let t0 = std::time::Instant::now();
            // Only live listeners reach this probe, so the budget is paid
            // per Chrome, not per scanned port. Chrome under start-up
            // contention answers /json/version in seconds.
            let cmdline = query_chrome_cmdline(port, PROBE_TIMEOUT).await;
            trace(|| {
                format!(
                    "probe {port}: {:?} in {:?}",
                    cmdline.as_ref().map(Vec::len),
                    t0.elapsed()
                )
            });
            let cmdline = cmdline?;
            let workspace = extract_workspace(&cmdline);
            let pid = pid_on_port_async(port).await;
            Some(FoundChrome {
                port,
                pid,
                workspace,
            })
        }));
    }
    let mut out = Vec::new();
    for h in handles {
        if let Ok(Some(c)) = h.await {
            out.push(c);
        }
    }
    out.sort_by_key(|c| c.port);
    out
}

fn range_fully_unbindable(range: (u16, u16)) -> bool {
    !(range.0..range.1).any(|p| is_port_bindable(DEFAULT_DEBUG_HOST, p))
}

/// OS dynamic/ephemeral port range, for the Hyper-V slow path.
fn os_ephemeral_range() -> (u16, u16) {
    let fallback = (1024, u16::MAX);
    if cfg!(target_os = "linux") {
        if let Ok(s) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
            let parts: Vec<u16> = s
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect();
            if parts.len() == 2 {
                return (parts[0], parts[1].saturating_add(1));
            }
        }
    } else if cfg!(target_os = "macos") {
        let read = |key: &str| -> Option<u16> {
            let out = std::process::Command::new("sysctl")
                .args(["-n", key])
                .output()
                .ok()?;
            String::from_utf8_lossy(&out.stdout).trim().parse().ok()
        };
        if let (Some(first), Some(last)) = (
            read("net.inet.ip.portrange.first"),
            read("net.inet.ip.portrange.last"),
        ) {
            return (first, last.saturating_add(1));
        }
    } else if cfg!(windows) {
        if let Ok(out) = std::process::Command::new("netsh")
            .args(["int", "ipv4", "show", "dynamicport", "tcp"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            let grab = |label: &str| -> Option<u32> {
                text.lines()
                    .find(|l| l.contains(label))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse().ok())
            };
            if let (Some(start), Some(num)) = (grab("Start Port"), grab("Number of Ports")) {
                return (start as u16, (start + num).min(65535) as u16);
            }
        }
    }
    fallback
}

/// Find all debugging Chromes. Fast path: the preferred band; slow path
/// (only when the band is fully unbindable — the Hyper-V fingerprint): the
/// OS ephemeral range.
pub async fn find_debug_chromes(range: (u16, u16)) -> Vec<FoundChrome> {
    let found = scan_ports_for_chrome(range).await;
    if !found.is_empty() {
        return found;
    }
    if range == DEFAULT_PORT_RANGE && range_fully_unbindable(range) {
        return scan_ports_for_chrome(os_ephemeral_range()).await;
    }
    found
}

/// Debugging Chromes whose workspace tag matches `workspace`.
pub async fn find_workspace_chromes(workspace: &Path, range: (u16, u16)) -> Vec<FoundChrome> {
    let want = normalize_workspace(workspace);
    find_debug_chromes(range)
        .await
        .into_iter()
        .filter(|c| {
            c.workspace
                .as_deref()
                .is_some_and(|w| normalize_workspace(Path::new(w)) == want)
        })
        .collect()
}

fn allocate_ephemeral_port(exclude: &[u16]) -> Result<u16> {
    for _ in 0..10 {
        let l = TcpListener::bind(addr(DEFAULT_DEBUG_HOST, 0))?;
        let p = l.local_addr()?.port();
        if !exclude.contains(&p) {
            return Ok(p);
        }
    }
    Err(crate::Error::Chrome(
        "Could not obtain an ephemeral port not in exclude set".to_string(),
    ))
}

/// First bindable port in the preferred band, else an OS-assigned one.
pub fn get_available_port(range: (u16, u16), exclude: &[u16]) -> Result<u16> {
    for p in range.0..range.1 {
        if exclude.contains(&p) {
            continue;
        }
        if is_port_bindable(DEFAULT_DEBUG_HOST, p) {
            return Ok(p);
        }
    }
    allocate_ephemeral_port(exclude)
}

/// PID of the process listening on `port` (`lsof` on Unix, `netstat` on Windows).
#[must_use]
pub fn get_pid_on_port(port: u16) -> Option<u32> {
    if cfg!(windows) {
        let out = std::process::Command::new("netstat")
            .arg("-ano")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let needle = format!(":{port}");
        for line in text.lines() {
            if line.contains(&needle) && line.contains("LISTENING") && line.contains("TCP") {
                if let Some(pid) = line.split_whitespace().last().and_then(|p| p.parse().ok()) {
                    return Some(pid);
                }
            }
        }
        None
    } else {
        let out = std::process::Command::new("lsof")
            .args(["-i", &format!(":{port}"), "-t", "-sTCP:LISTEN"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .and_then(|l| l.trim().parse().ok())
    }
}

/// [`get_pid_on_port`] off the async runtime. `lsof` can take seconds on a
/// loaded machine; run inline it would stall every other probe's timer.
pub async fn pid_on_port_async(port: u16) -> Option<u32> {
    tokio::task::spawn_blocking(move || get_pid_on_port(port))
        .await
        .ok()
        .flatten()
}

/// Kill a process and its whole tree (`taskkill /T` on Windows; SIGKILL to
/// the process group on Unix — Chrome is launched in its own group).
#[must_use]
pub fn kill_process_tree(pid: u32) -> bool {
    let status = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
    } else {
        let group = std::process::Command::new("kill")
            .args(["-9", "--", &format!("-{pid}")])
            .output();
        match group {
            Ok(o) if o.status.success() => Ok(o),
            _ => std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output(),
        }
    };
    status.is_ok()
}

/// Remove temp profile dirs for `port` (`{prefix}{port}_*` and legacy `{prefix}{port}`).
#[must_use]
pub fn cleanup_temp_profile(port: u16) -> bool {
    crate::registry::remove(port);
    let tmp = std::env::temp_dir();
    let prefix = format!("{DEFAULT_PROFILE_PREFIX}{port}_");
    let legacy = format!("{DEFAULT_PROFILE_PREFIX}{port}");
    let mut cleaned = false;
    if let Ok(entries) = std::fs::read_dir(&tmp) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if (name.starts_with(&prefix) || name == legacy)
                && std::fs::remove_dir_all(e.path()).is_ok()
            {
                cleaned = true;
            }
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_extraction() {
        let args = vec![
            "--foo".to_string(),
            "--ai-dev-browser-workspace=\"/a/b\"".to_string(),
        ];
        assert_eq!(extract_workspace(&args).as_deref(), Some("/a/b"));
        assert_eq!(extract_workspace(&["--x".to_string()]), None);
    }

    #[test]
    fn free_port_is_not_in_use() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        assert!(is_port_in_use(p));
        drop(l);
        assert!(!is_port_in_use(p));
    }
}
