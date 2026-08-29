//! Extensions: `suh <name> …` for any `<name>` that is not a built-in domain
//! runs the executable `suh-<name>` — the git / cargo / kubectl convention.
//!
//! An extension packages app-specific operations (say `suh wx send …` for
//! WeChat) on top of the four actuators. It can be written in any language:
//! a shell script that calls back into `$SUH_BIN desktop …`, or a Rust
//! binary that links the `sudohand-*` crates directly (see
//! `sudoprivacy/suh-wx`). Either way it is expected to follow the same
//! contract as the built-ins: JSON on stdout, `{"error":{kind,message}}`
//! on stderr, exit 1 on failure.
//!
//! Lookup order for `suh-<name>` (first hit wins):
//!
//! 1. every directory in `$SUH_EXT_PATH` (colon-separated);
//! 2. `~/.suh/extensions/suh-<name>` and `~/.suh/extensions/<name>/suh-<name>`;
//! 3. the directory holding the running `suh` executable;
//! 4. every directory in `$PATH`.
//!
//! The child inherits stdio and the environment, plus `SUH_BIN` (absolute
//! path of this `suh`) and `SUH_EXT_NAME`. Its exit status is propagated.

use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use sudohand_core::{Error, Result};

const PREFIX: &str = "suh-";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Extension {
    pub name: String,
    pub path: PathBuf,
    /// Where it was found: `ext_path`, `home`, `sibling` or `PATH`.
    pub source: &'static str,
}

/// Directories searched, in priority order, each tagged with its source.
/// Pure over `env`, `home` and `exe_dir` so tests can drive it.
fn search_dirs(
    ext_path: Option<&str>,
    home: Option<&Path>,
    exe_dir: Option<&Path>,
    path: Option<&str>,
) -> Vec<(PathBuf, &'static str)> {
    let mut dirs = Vec::new();
    if let Some(p) = ext_path {
        dirs.extend(
            std::env::split_paths(p)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| (d, "ext_path")),
        );
    }
    if let Some(h) = home {
        dirs.push((h.join(".suh").join("extensions"), "home"));
    }
    if let Some(d) = exe_dir {
        dirs.push((d.to_path_buf(), "sibling"));
    }
    if let Some(p) = path {
        dirs.extend(
            std::env::split_paths(p)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| (d, "PATH")),
        );
    }
    dirs
}

fn env_dirs() -> Vec<(PathBuf, &'static str)> {
    let ext_path = std::env::var("SUH_EXT_PATH").ok();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf));
    let path = std::env::var("PATH").ok();
    search_dirs(
        ext_path.as_deref(),
        home.as_deref(),
        exe_dir.as_deref(),
        path.as_deref(),
    )
}

/// `[a-z0-9][a-z0-9_-]*` — what can follow `suh-` in a file name and be
/// typed as a subcommand.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Candidate files for `name` inside one search directory: `dir/suh-name`
/// and `dir/name/suh-name` (the per-extension folder layout).
fn candidates(dir: &Path, name: &str) -> [PathBuf; 2] {
    let file = format!("{PREFIX}{name}");
    [dir.join(&file), dir.join(name).join(&file)]
}

fn find_in(dirs: &[(PathBuf, &'static str)], name: &str) -> Option<Extension> {
    if !valid_name(name) {
        return None;
    }
    dirs.iter().find_map(|(dir, source)| {
        candidates(dir, name)
            .into_iter()
            .find(|p| is_executable(p))
            .map(|path| Extension {
                name: name.to_string(),
                path,
                source,
            })
    })
}

fn list_in(dirs: &[(PathBuf, &'static str)]) -> Vec<Extension> {
    let mut found: BTreeMap<String, Extension> = BTreeMap::new();
    for (dir, source) in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(fname) = file_name.to_str() else {
                continue;
            };
            let p = entry.path();
            // `dir/suh-<name>` …
            if let Some(name) = fname.strip_prefix(PREFIX) {
                if valid_name(name) && is_executable(&p) {
                    found.entry(name.to_string()).or_insert(Extension {
                        name: name.to_string(),
                        path: p,
                        source,
                    });
                }
                continue;
            }
            // … or `dir/<name>/suh-<name>`.
            if valid_name(fname) && p.is_dir() {
                let inner = p.join(format!("{PREFIX}{fname}"));
                if is_executable(&inner) {
                    found.entry(fname.to_string()).or_insert(Extension {
                        name: fname.to_string(),
                        path: inner,
                        source,
                    });
                }
            }
        }
    }
    found.into_values().collect()
}

/// Resolve `suh-<name>` through the search order.
pub fn find(name: &str) -> Option<Extension> {
    find_in(&env_dirs(), name)
}

/// Every extension visible through the search order, one entry per name
/// (the first hit in priority order), sorted by name.
pub fn list() -> Vec<Extension> {
    list_in(&env_dirs())
}

/// `suh <name> args…` → run the extension. Returns the child's exit code.
pub fn exec(name: &str, args: &[OsString]) -> ExitCode {
    match find(name) {
        Some(ext) => run(&ext, args),
        None => {
            let err = if valid_name(name) {
                Error::not_found(format!(
                    "unknown command {name:?}: not a built-in domain and no `suh-{name}` \
                     extension found (searched $SUH_EXT_PATH, ~/.suh/extensions, the suh \
                     binary's directory and $PATH; `suh ext list` shows what is installed)"
                ))
            } else {
                Error::invalid(format!(
                    "unknown command {name:?} (extension names are [a-z0-9][a-z0-9_-]*)"
                ))
            };
            sudohand_core::print_result::<()>(Err(err))
        }
    }
}

fn command(ext: &Extension, args: &[OsString]) -> std::process::Command {
    let mut cmd = std::process::Command::new(&ext.path);
    cmd.args(args).env("SUH_EXT_NAME", &ext.name);
    if let Ok(me) = std::env::current_exe() {
        cmd.env("SUH_BIN", me);
    }
    cmd
}

#[cfg(unix)]
fn run(ext: &Extension, args: &[OsString]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    // Replace this process so signals, tty and exit status all belong to the
    // extension. `exec` only returns on failure.
    let err = command(ext, args).exec();
    sudohand_core::print_result::<()>(Err(Error::io(format!(
        "cannot run {}: {err}",
        ext.path.display()
    ))))
}

#[cfg(not(unix))]
fn run(ext: &Extension, args: &[OsString]) -> ExitCode {
    match command(ext, args).status() {
        Ok(st) => match st.code() {
            Some(c) => ExitCode::from(c.clamp(0, 255) as u8),
            None => ExitCode::FAILURE,
        },
        Err(err) => sudohand_core::print_result::<()>(Err(Error::io(format!(
            "cannot run {}: {err}",
            ext.path.display()
        )))),
    }
}

/// `suh ext …` — introspection over the extension search path.
#[derive(clap::Subcommand)]
pub enum Cmd {
    /// List every extension found on the search path (name, path, source).
    List,
    /// Resolve one extension name to the executable that `suh <name>` would run.
    Which { name: String },
    /// Print the directories searched, in priority order.
    Path,
    /// Print an extension's manifest (`suh-<name> --manifest`).
    Info { name: String },
    /// Verify an extension against the contract: `--manifest`, `--help`,
    /// and the error envelope. Exit 1 with the failing checks otherwise.
    Check { name: String },
}

pub fn run_cmd(cmd: Cmd) -> Result<serde_json::Value> {
    use serde_json::json;
    Ok(match cmd {
        Cmd::List => json!({ "extensions": list() }),
        Cmd::Which { name } => match find(&name) {
            Some(ext) => json!(ext),
            None => return Err(Error::not_found(format!("no `suh-{name}` extension found"))),
        },
        Cmd::Info { name } => {
            let ext = find(&name)
                .ok_or_else(|| Error::not_found(format!("no `suh-{name}` extension found")))?;
            let out = std::process::Command::new(&ext.path)
                .arg("--manifest")
                .env("SUH_FAKE", "1")
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| Error::io(format!("cannot run {}: {e}", ext.path.display())))?;
            if !out.status.success() {
                return Err(Error::io(format!(
                    "`{} --manifest` failed: {}",
                    ext.path.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let m: sudohand_ext::Manifest = serde_json::from_slice(&out.stdout).map_err(|e| {
                Error::io(format!(
                    "{}: manifest is not valid: {e}",
                    ext.path.display()
                ))
            })?;
            json!({ "path": ext.path, "source": ext.source, "manifest": m })
        }
        Cmd::Check { name } => {
            let ext = find(&name)
                .ok_or_else(|| Error::not_found(format!("no `suh-{name}` extension found")))?;
            let report = sudohand_ext::check::run(&ext.path);
            if !report.ok {
                return Err(Error::invalid(format!(
                    "{} failed the extension contract:\n{}",
                    ext.path.display(),
                    report.failures()
                )));
            }
            json!(report)
        }
        Cmd::Path => json!({
            "dirs": env_dirs()
                .into_iter()
                .map(|(d, s)| json!({ "dir": d, "source": s }))
                .collect::<Vec<_>>()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("suh-ext-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn script(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(p, "#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn names() {
        assert!(valid_name("wx"));
        assert!(valid_name("my-app_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("-x"));
        assert!(!valid_name("Wx"));
        assert!(!valid_name("a/b"));
    }

    #[test]
    fn search_order() {
        let dirs = search_dirs(
            Some("/a:/b"),
            Some(Path::new("/home/u")),
            Some(Path::new("/opt/suh")),
            Some("/usr/bin"),
        );
        let got: Vec<(String, &str)> = dirs
            .iter()
            .map(|(d, s)| (d.display().to_string(), *s))
            .collect();
        assert_eq!(
            got,
            [
                ("/a".to_string(), "ext_path"),
                ("/b".to_string(), "ext_path"),
                ("/home/u/.suh/extensions".to_string(), "home"),
                ("/opt/suh".to_string(), "sibling"),
                ("/usr/bin".to_string(), "PATH"),
            ]
        );
        assert!(search_dirs(None, None, None, None).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn find_and_list_with_priority_and_folder_layout() {
        let hi = tmp("hi");
        let lo = tmp("lo");
        // flat file in the high-priority dir
        script(&hi.join("suh-wx"));
        // same name lower down must lose; a folder-layout one must be found
        script(&lo.join("suh-wx"));
        std::fs::create_dir_all(lo.join("mail")).unwrap();
        script(&lo.join("mail").join("suh-mail"));
        // not executable → ignored; bad name → ignored
        std::fs::write(lo.join("suh-noexec"), "x").unwrap();
        script(&lo.join("suh-Bad"));

        let dirs = vec![(hi.clone(), "ext_path"), (lo.clone(), "home")];
        let wx = find_in(&dirs, "wx").unwrap();
        assert_eq!(
            (wx.path.clone(), wx.source),
            (hi.join("suh-wx"), "ext_path")
        );
        let mail = find_in(&dirs, "mail").unwrap();
        assert_eq!(mail.path, lo.join("mail").join("suh-mail"));
        assert!(find_in(&dirs, "noexec").is_none());
        assert!(find_in(&dirs, "Bad").is_none());
        assert!(find_in(&dirs, "missing").is_none());

        let names: Vec<(String, &str)> = list_in(&dirs)
            .into_iter()
            .map(|e| (e.name, e.source))
            .collect();
        assert_eq!(
            names,
            [("mail".to_string(), "home"), ("wx".to_string(), "ext_path")]
        );

        let _ = std::fs::remove_dir_all(hi);
        let _ = std::fs::remove_dir_all(lo);
    }
}
