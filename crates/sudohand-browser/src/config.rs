//! Shared configuration: env vars, paths, defaults. Mirrors `core/config.py`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Env var: absolute path to a Chrome executable.
pub const CHROME_ENV: &str = "AI_DEV_BROWSER_CHROME";
/// Env var: debug port — short-circuits every scan.
pub const PORT_ENV: &str = "AI_DEV_BROWSER_PORT";
/// Env var: `1`/`true`/`new`/`old` headless default for `browser_start`.
pub const HEADLESS_ENV: &str = "AI_DEV_BROWSER_HEADLESS";
/// Env var: directory for file-producing tools.
pub const OUTPUT_DIR_ENV: &str = "AI_DEV_BROWSER_OUTPUT_DIR";
/// Env var: URL substring pinning which tab tools act on.
pub const TAB_URL_ENV: &str = "AI_DEV_BROWSER_TAB_URL";
/// Env var: `WxH` render viewport, or `native`/`off` to leave Chrome alone.
pub const VIEWPORT_ENV: &str = "AI_DEV_BROWSER_VIEWPORT";
/// Env var: `dismiss` (default) / `accept` / `off` auto-dialog policy.
pub const DIALOG_ENV: &str = "AI_DEV_BROWSER_DIALOG";

/// Temp profile prefix (identifies our Chrome instances).
pub const DEFAULT_PROFILE_PREFIX: &str = "ai_dev_browser_";
/// Chrome flag carrying the owning workspace, read back via
/// `Browser.getBrowserCommandLine`.
pub const WORKSPACE_FLAG: &str = "--ai-dev-browser-workspace";

/// Debug host.
pub const DEFAULT_DEBUG_HOST: &str = "127.0.0.1";
/// Default debug port.
pub const DEFAULT_DEBUG_PORT: u16 = 9350;
/// Preferred port band `[start, end)`.
pub const DEFAULT_PORT_RANGE: (u16, u16) = (9350, 9450);

/// Default render viewport width.
pub const DEFAULT_VIEWPORT_WIDTH: u32 = 1600;
/// Default render viewport height.
pub const DEFAULT_VIEWPORT_HEIGHT: u32 = 950;
/// Width at/above which a tab is already "desktop" and left untouched.
pub const DESKTOP_MIN_WIDTH: f64 = 1000.0;

/// `~/.ai-dev-browser`.
#[must_use]
pub fn base_dir() -> PathBuf {
    home_dir().join(".ai-dev-browser")
}

/// `~/.ai-dev-browser/profiles`.
#[must_use]
pub fn profile_root() -> PathBuf {
    base_dir().join("profiles")
}

/// The user's home directory (`HOME` / `USERPROFILE`).
#[must_use]
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(p);
        }
    }
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Directory that file-producing tools write to when `path` is omitted:
/// `$AI_DEV_BROWSER_OUTPUT_DIR` → `./output`.
#[must_use]
pub fn resolve_output_dir() -> PathBuf {
    match std::env::var(OUTPUT_DIR_ENV) {
        Ok(v) if !v.is_empty() => expand_tilde(&v),
        _ => PathBuf::from("output"),
    }
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        home_dir().join(rest)
    } else if s == "~" {
        home_dir()
    } else {
        PathBuf::from(s)
    }
}

/// Render viewport applied to every tab, or `None` to leave Chrome's native
/// viewport untouched. A malformed value is an error, not a silent fallback.
pub fn resolve_viewport() -> crate::Result<Option<(u32, u32)>> {
    let raw = std::env::var(VIEWPORT_ENV).unwrap_or_default();
    let raw = raw.trim().to_lowercase();
    if raw.is_empty() {
        return Ok(Some((DEFAULT_VIEWPORT_WIDTH, DEFAULT_VIEWPORT_HEIGHT)));
    }
    if matches!(raw.as_str(), "native" | "off" | "0" | "none" | "false") {
        return Ok(None);
    }
    let parsed = raw
        .split(['x', '*', '×'])
        .map(str::trim)
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|v| v.len() == 2 && v[0] > 0 && v[1] > 0);
    match parsed {
        Some(v) => Ok(Some((v[0], v[1]))),
        None => Err(crate::Error::Invalid(format!(
            "{VIEWPORT_ENV}={raw:?} is invalid — expected 'WIDTHxHEIGHT' (e.g. '1600x950') or 'native' to disable"
        ))),
    }
}

/// Auto-dialog policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogPolicy {
    /// Cancel/No — fail-safe default.
    Dismiss,
    /// OK/Yes.
    Accept,
}

/// `AI_DEV_BROWSER_DIALOG`: `dismiss` (default), `accept`, or `None` (off).
/// Unknown values fall back to `Dismiss`.
#[must_use]
pub fn resolve_dialog_policy() -> Option<DialogPolicy> {
    let raw = std::env::var(DIALOG_ENV).unwrap_or_default();
    match raw.trim().to_lowercase().as_str() {
        "off" | "manual" | "none" | "0" | "false" => None,
        "accept" => Some(DialogPolicy::Accept),
        _ => Some(DialogPolicy::Dismiss),
    }
}

/// `AI_DEV_BROWSER_PORT`, if set and numeric.
#[must_use]
pub fn env_port() -> Option<u16> {
    std::env::var(PORT_ENV).ok()?.trim().parse().ok()
}

/// Lexical `os.path.normpath`: collapse `.` / `..` / repeated separators.
#[must_use]
pub fn normpath(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                let popped = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if popped {
                    out.pop();
                } else if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `os.path.normcase(os.path.normpath(p))` — lower-case + backslashes on
/// Windows, identity elsewhere.
#[must_use]
pub fn normalize_workspace(p: &Path) -> String {
    let n = normpath(p).to_string_lossy().into_owned();
    if cfg!(windows) {
        n.replace('/', "\\").to_lowercase()
    } else {
        n
    }
}

/// Convert a workspace path into a filesystem-safe slug, byte-identical to
/// Python's `get_workspace_slug`: `/home/user/project-a` → `home_user_project-a_a1b2c3`.
#[must_use]
pub fn workspace_slug(workspace: &Path) -> String {
    let normalized = normalize_workspace(workspace);
    let cleaned = normalized.replace(':', "");
    let slug: String = cleaned
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let slug = slug.trim_matches('_');
    let hex = hex_digest(normalized.as_bytes());
    let short = &hex[..6];
    let slug = if slug.len() > 60 {
        slug[..60].trim_end_matches('_')
    } else {
        slug
    };
    format!("{slug}_{short}")
}

/// Lower-case hex SHA-256.
#[must_use]
pub fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// Current working directory as the workspace.
#[must_use]
pub fn current_workspace() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// `~/.ai-dev-browser/profiles/{workspace_slug}/{profile}`.
#[must_use]
pub fn workspace_profile_dir(profile: &str, workspace: Option<&Path>) -> PathBuf {
    let ws = workspace.map_or_else(current_workspace, Path::to_path_buf);
    profile_root().join(workspace_slug(&ws)).join(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_python_reference() {
        // python3 -c 'from ai_dev_browser.core.config import get_workspace_slug; print(get_workspace_slug("/home/user/project-a"))'
        // → home_user_project-a_<sha256("/home/user/project-a")[:6]>
        let s = workspace_slug(Path::new("/home/user/project-a"));
        // On Windows normcase lower-cases and flips separators before hashing.
        let hex = hex_digest(normalize_workspace(Path::new("/home/user/project-a")).as_bytes());
        assert_eq!(s, format!("home_user_project-a_{}", &hex[..6]));
    }

    #[test]
    fn slug_truncates_at_60() {
        let long = format!("/{}", "a".repeat(100));
        let s = workspace_slug(Path::new(&long));
        let (head, _) = s.rsplit_once('_').unwrap();
        assert_eq!(head.len(), 60);
    }

    #[test]
    fn normpath_collapses() {
        assert_eq!(normpath(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normpath(Path::new("/a//b/")), PathBuf::from("/a/b"));
    }

    #[test]
    fn viewport_parsing() {
        std::env::remove_var(VIEWPORT_ENV);
        assert_eq!(resolve_viewport().unwrap(), Some((1600, 950)));
    }
}
