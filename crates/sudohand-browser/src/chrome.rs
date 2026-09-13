//! Chrome executable detection and launching. Mirrors `core/chrome.py`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::config::{CHROME_ENV, DEFAULT_PROFILE_PREFIX, WORKSPACE_FLAG};
use crate::{Error, Result};

/// Find the Chrome / Chromium / Edge executable.
///
/// Order: `AI_DEV_BROWSER_CHROME` → known per-platform paths → `PATH` lookup
/// (Unix). Unlike the Python original, the Windows table includes
/// Microsoft Edge (`msedge.exe`).
#[must_use]
pub fn find_chrome() -> Option<PathBuf> {
    if let Ok(env) = std::env::var(CHROME_ENV) {
        let p = PathBuf::from(&env);
        if !env.is_empty() && p.is_file() {
            return Some(p);
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();

    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
            home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        ]
    } else if cfg!(windows) {
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        let pf86 =
            std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        let local = std::env::var("LOCALAPPDATA")
            .map_or_else(|_| home.join(r"AppData\Local"), PathBuf::from);
        vec![
            PathBuf::from(&pf).join(r"Google\Chrome\Application\chrome.exe"),
            PathBuf::from(&pf86).join(r"Google\Chrome\Application\chrome.exe"),
            local.join(r"Google\Chrome\Application\chrome.exe"),
            PathBuf::from(&pf86).join(r"Microsoft\Edge\Application\msedge.exe"),
            PathBuf::from(&pf).join(r"Microsoft\Edge\Application\msedge.exe"),
            local.join(r"Microsoft\Edge\Application\msedge.exe"),
        ]
    } else {
        vec![
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/usr/bin/chromium-browser"),
            PathBuf::from("/snap/bin/chromium"),
        ]
    };
    for c in candidates {
        if c.exists() {
            return Some(c);
        }
    }
    if !cfg!(windows) {
        for cmd in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ] {
            if let Some(p) = which(cmd) {
                return Some(p);
            }
        }
    }
    None
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
}

/// Headless mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Headless {
    /// Windowed Chrome.
    #[default]
    Off,
    /// `--headless=new`.
    New,
    /// Legacy `--headless`.
    Old,
}

impl Headless {
    /// Parse `1`/`true`/`new`/`old`/anything-else the way the Python env
    /// handling does.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "1" | "true" | "new" => Self::New,
            "old" => Self::Old,
            _ => Self::Off,
        }
    }

    /// True for either headless mode.
    #[must_use]
    pub fn enabled(self) -> bool {
        self != Self::Off
    }
}

/// Launch options.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Remote debugging port.
    pub port: u16,
    /// Headless mode.
    pub headless: Headless,
    /// Omit automation marker flags by default.
    pub stealth: bool,
    /// Profile dir; `None` → fresh temp dir per launch.
    pub user_data_dir: Option<PathBuf>,
    /// Extra flags appended after the defaults.
    pub extra_args: Vec<String>,
    /// Override (`Some`) or remove (`None`) default flags, matched by prefix.
    pub override_default_args: Vec<(String, Option<String>)>,
    /// Route stderr to null instead of a pipe.
    pub silent_stderr: bool,
    /// Initial URL.
    pub start_url: String,
    /// Workspace tag written into `--ai-dev-browser-workspace=`.
    pub workspace: PathBuf,
    /// Render viewport for the windowed `--window-size` flag.
    pub window_size: Option<(u32, u32)>,
}

impl LaunchOptions {
    /// Sensible defaults for `port`.
    #[must_use]
    pub fn new(port: u16) -> Self {
        Self {
            port,
            headless: Headless::Off,
            stealth: true,
            user_data_dir: None,
            extra_args: Vec::new(),
            override_default_args: Vec::new(),
            silent_stderr: false,
            start_url: "about:blank".to_string(),
            workspace: crate::config::current_workspace(),
            window_size: None,
        }
    }
}

/// A launched Chrome.
#[derive(Debug)]
pub struct Launched {
    /// Process handle.
    pub child: Child,
    /// The profile dir in use.
    pub user_data_dir: PathBuf,
    /// Executable used.
    pub chrome_path: PathBuf,
}

/// Set `session.restore_on_startup = 5` in the profile's Preferences.
fn ensure_no_session_restore(user_data_dir: &Path) -> Result<()> {
    let default_dir = user_data_dir.join("Default");
    std::fs::create_dir_all(&default_dir)?;
    let prefs_file = default_dir.join("Preferences");
    let mut prefs: serde_json::Value = std::fs::read_to_string(&prefs_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !prefs.is_object() {
        prefs = serde_json::json!({});
    }
    let session = prefs
        .as_object_mut()
        .expect("object")
        .entry("session")
        .or_insert_with(|| serde_json::json!({}));
    if !session.is_object() {
        *session = serde_json::json!({});
    }
    session["restore_on_startup"] = serde_json::json!(5);
    std::fs::write(&prefs_file, serde_json::to_string(&prefs)?)?;
    Ok(())
}

fn temp_profile_dir(port: u16) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "{DEFAULT_PROFILE_PREFIX}{port}_{:x}{:x}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Build the argv (without the executable) for `opts`.
#[must_use]
pub fn build_args(opts: &LaunchOptions, user_data_dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        format!("--remote-debugging-port={}", opts.port),
        format!("--user-data-dir={}", user_data_dir.display()),
        "--remote-allow-origins=*".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--disable-client-side-phishing-detection".into(),
        "--disable-default-apps".into(),
        "--disable-extensions".into(),
        "--disable-hang-monitor".into(),
        "--disable-popup-blocking".into(),
        "--disable-prompt-on-repost".into(),
        "--disable-sync".into(),
        "--disable-translate".into(),
        "--metrics-recording-only".into(),
        "--safebrowsing-disable-auto-update".into(),
        "--use-mock-keychain".into(),
    ];
    if !opts.stealth {
        args.push("--enable-automation".into());
        args.push("--disable-blink-features=AutomationControlled".into());
    }
    if !opts.headless.enabled() {
        if let Some((w, h)) = opts.window_size {
            args.push(format!("--window-size={w},{h}"));
        }
    }
    args.push(format!("{WORKSPACE_FLAG}={}", opts.workspace.display()));
    args.push("--disable-session-crashed-bubble".into());
    args.push("--hide-crash-restore-bubble".into());
    match opts.headless {
        Headless::Off => {}
        Headless::New => args.push("--headless=new".into()),
        Headless::Old => args.push("--headless".into()),
    }
    if !opts.override_default_args.is_empty() {
        let keys: Vec<&str> = opts
            .override_default_args
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        args.retain(|a| {
            !keys
                .iter()
                .any(|k| a == k || a.starts_with(&format!("{k}=")))
        });
        for (k, v) in &opts.override_default_args {
            match v {
                None => {}
                Some(val) if val.is_empty() => args.push(k.clone()),
                Some(val) => args.push(format!("{k}={val}")),
            }
        }
    }
    args.extend(opts.extra_args.iter().cloned());
    if !opts.start_url.is_empty() {
        args.push(opts.start_url.clone());
    }
    args
}

/// Launch Chrome with remote debugging enabled.
pub fn launch_chrome(opts: &LaunchOptions) -> Result<Launched> {
    let chrome_path = find_chrome().ok_or_else(|| {
        Error::Chrome(
            "Chrome executable not found. Please install Google Chrome or set AI_DEV_BROWSER_CHROME."
                .to_string(),
        )
    })?;
    let user_data_dir = match &opts.user_data_dir {
        Some(d) => d.clone(),
        None => temp_profile_dir(opts.port)?,
    };
    ensure_no_session_restore(&user_data_dir)?;
    let args = build_args(opts, &user_data_dir);

    let mut cmd = Command::new(&chrome_path);
    cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::null());
    cmd.stderr(if opts.silent_stderr {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    let child = cmd
        .spawn()
        .map_err(|e| Error::Chrome(format!("Failed to launch Chrome: {e}")))?;
    Ok(Launched {
        child,
        user_data_dir,
        chrome_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_bearing_flags_present() {
        let opts = LaunchOptions::new(9999);
        let args = build_args(&opts, Path::new("/tmp/p"));
        assert!(args.contains(&"--remote-debugging-port=9999".to_string()));
        assert!(args.contains(&"--user-data-dir=/tmp/p".to_string()));
        assert!(args.contains(&"--remote-allow-origins=*".to_string()));
        assert!(!args.contains(&"--enable-automation".to_string()));
        assert!(!args.contains(&"--disable-blink-features=AutomationControlled".to_string()));
        let mut legacy = opts.clone();
        legacy.stealth = false;
        let legacy_args = build_args(&legacy, Path::new("/tmp/p"));
        assert!(legacy_args.contains(&"--enable-automation".to_string()));
        assert!(legacy_args.contains(&"--disable-blink-features=AutomationControlled".to_string()));
        assert!(args.iter().any(|a| a.starts_with(WORKSPACE_FLAG)));
        assert_eq!(args.last().unwrap(), "about:blank");
    }

    #[test]
    fn override_removes_and_replaces() {
        let mut opts = LaunchOptions::new(1);
        opts.override_default_args = vec![
            ("--disable-extensions".into(), None),
            ("--remote-allow-origins".into(), Some("localhost".into())),
        ];
        let args = build_args(&opts, Path::new("/x"));
        assert!(!args.contains(&"--disable-extensions".to_string()));
        assert!(!args.contains(&"--remote-allow-origins=*".to_string()));
        assert!(args.contains(&"--remote-allow-origins=localhost".to_string()));
    }

    #[test]
    fn headless_modes() {
        let mut opts = LaunchOptions::new(1);
        opts.headless = Headless::New;
        assert!(build_args(&opts, Path::new("/x")).contains(&"--headless=new".to_string()));
        opts.headless = Headless::Old;
        assert!(build_args(&opts, Path::new("/x")).contains(&"--headless".to_string()));
        assert_eq!(Headless::parse("true"), Headless::New);
        assert_eq!(Headless::parse("old"), Headless::Old);
        assert_eq!(Headless::parse(""), Headless::Off);
    }
}
