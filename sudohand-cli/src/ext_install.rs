//! `suh ext install|uninstall|update` — put an extension **next to `suh`**
//! (git-style `suh-<name>` in the same directory as the running binary),
//! where the search path finds it via `$PATH` — which stays visible even when
//! a sandbox has hidden `$HOME` (unlike the old `~/.suh/extensions/` home).
//!
//! Sources, told apart by looking at the argument:
//!
//! - an executable file → copied;
//! - a directory with a `Cargo.toml` → `cargo build --release`, the
//!   `suh-*` binary it produces is copied;
//! - `owner/repo[@ref]` or a git URL → cloned (or updated) into
//!   `~/.suh/src/<repo>`, then built like a project.
//!
//! The name comes from the binary's `--manifest`, never from the argument.
//! Before anything is written the binary must pass the conformance check
//! (`--force` skips that). Each install leaves a `suh-<name>.install.json`
//! record next to the binary (the bin dir is shared, so the record is named
//! per-binary), so `update` can redo it and `list` can show the version.

use crate::ext::{self, Extension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use sudohand_core::{Error, Result};
use sudohand_ext::Manifest;

pub const RECORD: &str = "install.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    File {
        path: PathBuf,
    },
    Project {
        path: PathBuf,
    },
    Git {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        checkout: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub name: String,
    pub version: String,
    pub source: Source,
    /// Unix seconds.
    pub installed_at: u64,
    pub manifest: Manifest,
}

/// `~/.suh`.
pub fn suh_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".suh"))
        .ok_or_else(|| Error::internal("HOME is not set"))
}

/// Where extensions install: the directory of the running `suh`, so a
/// `suh-<name>` lands next to `suh` on `$PATH` (visible even when a sandbox
/// hides `$HOME`). `$SUH_EXT_DIR` overrides it — for a read-only bin dir, or
/// tests; a custom dir must then also be on `$SUH_EXT_PATH`/`$PATH` for
/// dispatch to find it.
pub fn install_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("SUH_EXT_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .ok_or_else(|| Error::internal("cannot locate the `suh` binary's directory"))
}

/// The per-binary record path: `<bin>.install.json` next to the executable.
/// A bare `install.json` can't be used because the bin dir is shared.
fn record_path(bin: &Path) -> PathBuf {
    let file = bin
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    bin.with_file_name(format!("{file}.{RECORD}"))
}

/// Read an install record for `bin`: the per-binary `<bin>.install.json`
/// first, then the legacy `<dir>/install.json` (old `~/.suh/extensions`
/// layout) so pre-move installs still report their source.
pub fn record_for(bin: &Path) -> Option<Record> {
    let read =
        |p: PathBuf| -> Option<Record> { serde_json::from_slice(&std::fs::read(p).ok()?).ok() };
    read(record_path(bin)).or_else(|| read(bin.parent()?.join(RECORD)))
}

/// Parse the source argument. Local paths win when they exist; otherwise
/// `owner/repo[@ref]`, `https://…`, `git@…` and `….git` are git.
pub fn parse_source(arg: &str) -> Result<Source> {
    parse_source_in(arg, &suh_home()?)
}

fn parse_source_in(arg: &str, suh_home: &Path) -> Result<Source> {
    let p = Path::new(arg);
    if p.exists() {
        let path = p
            .canonicalize()
            .map_err(|e| Error::io(format!("{arg}: {e}")))?;
        return if path.is_dir() {
            if path.join("Cargo.toml").is_file() {
                Ok(Source::Project { path })
            } else {
                Err(Error::invalid(format!(
                    "{arg}: a directory without Cargo.toml; pass the executable or a cargo project"
                )))
            }
        } else {
            Ok(Source::File { path })
        };
    }
    let (spec, reference) = match arg.rsplit_once('@') {
        // `git@github.com:…` has an `@` that is not a ref separator.
        Some((s, r)) if !s.is_empty() && !r.contains('/') && !r.contains(':') => {
            (s, Some(r.to_string()))
        }
        _ => (arg, None),
    };
    let url = if spec.starts_with("https://")
        || spec.starts_with("http://")
        || spec.starts_with("git@")
        || spec.starts_with("ssh://")
        || spec.ends_with(".git")
    {
        spec.to_string()
    } else {
        let mut parts = spec.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(repo), None) if !owner.is_empty() && !repo.is_empty() => {
                format!("https://github.com/{owner}/{repo}")
            }
            _ => {
                return Err(Error::invalid(format!(
                    "{arg}: not a file, a cargo project, `owner/repo[@ref]` or a git URL"
                )))
            }
        }
    };
    let repo = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("ext")
        .to_string();
    Ok(Source::Git {
        url,
        reference,
        commit: None,
        checkout: suh_home.join("src").join(repo),
    })
}

fn run(cmd: &mut Command, what: &str) -> Result<String> {
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io(format!("{what}: cannot run: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(15).collect::<Vec<_>>();
        return Err(Error::io(format!(
            "{what} failed (exit {:?}):\n{}",
            out.status.code(),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Clone or update the checkout; returns the commit.
fn fetch_git(url: &str, reference: Option<&str>, checkout: &Path) -> Result<String> {
    if checkout.join(".git").is_dir() {
        run(
            Command::new("git")
                .args(["-C"])
                .arg(checkout)
                .args(["fetch", "--tags", "origin"]),
            "git fetch",
        )?;
        match reference {
            Some(r) => {
                run(
                    Command::new("git")
                        .arg("-C")
                        .arg(checkout)
                        .args(["checkout", "--detach", r]),
                    "git checkout",
                )?;
                // A branch name: move to its remote tip.
                let _ = Command::new("git")
                    .arg("-C")
                    .arg(checkout)
                    .args(["checkout", "--detach", &format!("origin/{r}")])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            None => {
                run(
                    Command::new("git")
                        .arg("-C")
                        .arg(checkout)
                        .args(["pull", "--ff-only"]),
                    "git pull",
                )?;
            }
        }
    } else {
        if let Some(parent) = checkout.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::from_io(&e))?;
        }
        let mut c = Command::new("git");
        c.args(["clone", "--quiet"]);
        if let Some(r) = reference {
            c.args(["--branch", r]);
        }
        c.arg(url).arg(checkout);
        run(&mut c, "git clone")?;
    }
    Ok(run(
        Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["rev-parse", "HEAD"]),
        "git rev-parse",
    )?
    .trim()
    .to_string())
}

/// `cargo build --release` and return the `suh-*` executable it produced.
fn build_project(dir: &Path) -> Result<PathBuf> {
    let out = run(
        Command::new("cargo")
            .args([
                "build",
                "--release",
                "--message-format=json-render-diagnostics",
            ])
            .current_dir(dir),
        "cargo build",
    )?;
    let mut bins: Vec<PathBuf> = out
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["reason"] == "compiler-artifact")
        .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
        .filter(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.starts_with("suh-"))
        })
        .collect();
    bins.dedup();
    match bins.len() {
        1 => Ok(bins.remove(0)),
        0 => Err(Error::not_found(format!(
            "{}: cargo built no `suh-*` binary",
            dir.display()
        ))),
        _ => Err(Error::invalid(format!(
            "{}: more than one `suh-*` binary: {}",
            dir.display(),
            bins.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Resolve a source to a built binary (filling in the git commit).
fn materialize(source: &mut Source) -> Result<PathBuf> {
    match source {
        Source::File { path } => Ok(path.clone()),
        Source::Project { path } => build_project(path),
        Source::Git {
            url,
            reference,
            commit,
            checkout,
        } => {
            *commit = Some(fetch_git(url, reference.as_deref(), checkout)?);
            build_project(checkout)
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Check, copy, record. Returns the install summary.
fn install_binary(bin: &Path, source: Source, force: bool) -> Result<serde_json::Value> {
    let report = sudohand_ext::check::run(bin);
    let manifest = match (report.manifest.clone(), report.ok || force) {
        (Some(m), true) => m,
        (None, _) => {
            return Err(Error::invalid(format!(
                "{}: no usable manifest, cannot install:\n{}",
                bin.display(),
                report.failures()
            )))
        }
        (Some(_), false) => {
            return Err(Error::invalid(format!(
                "{} failed the extension contract (use --force to install anyway):\n{}",
                bin.display(),
                report.failures()
            )))
        }
    };
    let name = manifest.name.clone();
    if !ext::valid_name(&name) {
        return Err(Error::invalid(format!(
            "manifest name {name:?} is not a valid extension name"
        )));
    }
    let dir = install_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| Error::from_io(&e))?;
    let dest = dir.join(format!("suh-{name}"));
    // Replace atomically-ish: write beside, then rename over (a running
    // copy keeps its old inode).
    let tmp = dir.join(format!(".suh-{name}.tmp"));
    std::fs::copy(bin, &tmp).map_err(|e| Error::io(format!("copy: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::from_io(&e))?;
    }
    std::fs::rename(&tmp, &dest).map_err(|e| Error::from_io(&e))?;
    let record = Record {
        name: name.clone(),
        version: manifest.version.clone(),
        source,
        installed_at: now(),
        manifest,
    };
    std::fs::write(
        record_path(&dest),
        serde_json::to_vec_pretty(&record).map_err(|e| Error::internal(e.to_string()))?,
    )
    .map_err(|e| Error::from_io(&e))?;
    // Is that what `suh <name>` will now run, or does something earlier on
    // the search path shadow it?
    let shadowed = ext::find(&name).filter(|e| e.path != dest).map(|e| e.path);
    Ok(json!({
        "installed": name,
        "version": record.version,
        "path": dest,
        "source": record.source,
        "checks": report.checks,
        "shadowed_by": shadowed,
    }))
}

pub fn install(arg: &str, force: bool) -> Result<serde_json::Value> {
    let mut source = parse_source(arg)?;
    let bin = materialize(&mut source)?;
    install_binary(&bin, source, force)
}

/// The managed install of `name`, if it was put there by `install`.
fn managed(name: &str) -> Result<(PathBuf, Option<Record>)> {
    if !ext::valid_name(name) {
        return Err(Error::invalid(format!(
            "{name:?} is not a valid extension name"
        )));
    }
    let bin = install_dir()?.join(format!("suh-{name}"));
    if !bin.is_file() {
        return Err(Error::not_found(match ext::find(name) {
            Some(Extension { path, .. }) => format!(
                "`suh-{name}` is not managed by `suh ext install` (found at {}); remove it yourself",
                path.display()
            ),
            None => format!("no installed extension `{name}`"),
        }));
    }
    let record = record_for(&bin);
    Ok((bin, record))
}

pub fn uninstall(name: &str) -> Result<serde_json::Value> {
    let (bin, record) = managed(name)?;
    std::fs::remove_file(&bin).map_err(|e| Error::from_io(&e))?;
    let _ = std::fs::remove_file(record_path(&bin));
    Ok(json!({
        "uninstalled": name,
        "path": bin,
        "version": record.map(|r| r.version),
    }))
}

pub fn update(name: &str, force: bool) -> Result<serde_json::Value> {
    let (_, record) = managed(name)?;
    let record = record.ok_or_else(|| {
        Error::not_found(format!(
            "`{name}` has no {RECORD}; reinstall it with `suh ext install <source>`"
        ))
    })?;
    let mut source = record.source;
    let bin = materialize(&mut source)?;
    let mut v = install_binary(&bin, source, force)?;
    v["previous_version"] = json!(record.version);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_parsing() {
        let home = Path::new("/home/u/.suh");
        let parse_source = |a: &str| parse_source_in(a, home);
        match parse_source("sudoprivacy/suh-wx@v1").unwrap() {
            Source::Git {
                url,
                reference,
                checkout,
                ..
            } => {
                assert_eq!(url, "https://github.com/sudoprivacy/suh-wx");
                assert_eq!(reference.as_deref(), Some("v1"));
                assert_eq!(checkout, Path::new("/home/u/.suh/src/suh-wx"));
            }
            other => panic!("{other:?}"),
        }
        match parse_source("git@github.com:sudoprivacy/suh-wx.git").unwrap() {
            Source::Git {
                url,
                reference,
                checkout,
                ..
            } => {
                assert_eq!(url, "git@github.com:sudoprivacy/suh-wx.git");
                assert_eq!(reference, None);
                assert!(checkout.ends_with("src/suh-wx"));
            }
            other => panic!("{other:?}"),
        }
        match parse_source("https://example.com/x/y.git@main").unwrap() {
            Source::Git { reference, .. } => assert_eq!(reference.as_deref(), Some("main")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_source("just-a-name"),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            parse_source(std::env::temp_dir().to_str().unwrap()),
            Err(Error::InvalidInput(_))
        ));
    }
}
