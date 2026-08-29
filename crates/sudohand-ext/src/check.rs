//! Black-box conformance check of one `suh-<name>` executable against the
//! extension contract. Used by `suh ext check <name>` and, via
//! [`assert_conformant`], by each extension's own test suite — so the
//! contract is enforced the same way for Rust and non-Rust extensions.
//!
//! Checks:
//! 1. `--manifest` → exit 0, stdout is a manifest with `schema` = 1 and
//!    `name` matching the file name, `commands` is a list;
//! 2. `--help` → exit 0;
//! 3. an unknown subcommand → exit 1, empty stdout, stderr is one
//!    `{"error":{"kind","message"}}` line with a known `kind`;
//! 4. `workflows` → exit 0, stdout is `{"workflows":[…]}` (may be empty).
//!
//! All runs get `SUH_FAKE=1` and a closed stdin, so a conformant extension
//! never touches the machine or waits for input.

use crate::manifest::{Manifest, SCHEMA};
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Stdio};

const KINDS: [&str; 5] = [
    "permission_denied",
    "not_found",
    "invalid_input",
    "io",
    "internal",
];

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub check: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub name: String,
    pub path: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Manifest>,
    pub checks: Vec<Check>,
}

impl Report {
    /// Every failing check as `check: detail`, one per line.
    pub fn failures(&self) -> String {
        self.checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| format!("{}: {}", c.check, c.detail))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn invoke(path: &Path, args: &[&str]) -> Result<Out, String> {
    let out = Command::new(path)
        .args(args)
        .env("SUH_FAKE", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", path.display()))?;
    Ok(Out {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Run every check against `path`, whose file name must be `suh-<name>`.
pub fn run(path: &Path) -> Report {
    let file = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default()
        .to_string();
    let name = file.strip_prefix("suh-").unwrap_or(&file).to_string();
    let mut report = Report {
        name: name.clone(),
        path: path.display().to_string(),
        ok: true,
        manifest: None,
        checks: Vec::new(),
    };
    let mut push = |check: &'static str, r: Result<String, String>| match r {
        Ok(detail) => report.checks.push(Check {
            check,
            ok: true,
            detail,
        }),
        Err(detail) => {
            report.ok = false;
            report.checks.push(Check {
                check,
                ok: false,
                detail,
            })
        }
    };

    // 1. --manifest
    let manifest = invoke(path, &["--manifest"]).and_then(|o| {
        if o.code != Some(0) {
            return Err(format!("exit {:?}, stderr: {}", o.code, o.stderr.trim()));
        }
        let m: Manifest = serde_json::from_str(&o.stdout)
            .map_err(|e| format!("stdout is not a manifest: {e}"))?;
        if m.schema != SCHEMA {
            return Err(format!("schema {} (expected {SCHEMA})", m.schema));
        }
        if m.name != name {
            return Err(format!("manifest name {:?} ≠ file name {name:?}", m.name));
        }
        Ok(m)
    });
    match manifest {
        Ok(m) => {
            push(
                "manifest",
                Ok(format!("{} command(s), v{}", m.commands.len(), m.version)),
            );
            report.manifest = Some(m);
        }
        Err(e) => push("manifest", Err(e)),
    }

    // 2. --help
    push(
        "help",
        invoke(path, &["--help"]).and_then(|o| {
            if o.code == Some(0) && !o.stdout.trim().is_empty() {
                Ok("exit 0".into())
            } else {
                Err(format!(
                    "exit {:?}, stdout {} bytes",
                    o.code,
                    o.stdout.len()
                ))
            }
        }),
    );

    // 3. unknown subcommand → error envelope
    push(
        "error_envelope",
        invoke(path, &["__suh_check_no_such_command__"]).and_then(|o| {
            if o.code != Some(1) {
                return Err(format!("exit {:?} (expected 1)", o.code));
            }
            if !o.stdout.trim().is_empty() {
                return Err("stdout not empty on error".into());
            }
            let v: serde_json::Value = serde_json::from_str(o.stderr.trim())
                .map_err(|_| format!("stderr is not JSON: {}", o.stderr.trim()))?;
            let kind = v["error"]["kind"].as_str().unwrap_or_default();
            if !KINDS.contains(&kind) {
                return Err(format!("error.kind {kind:?} not in {KINDS:?}"));
            }
            if v["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty()
            {
                return Err("error.message missing".into());
            }
            Ok(format!("kind={kind}"))
        }),
    );

    // 4. workflows
    push(
        "workflows",
        invoke(path, &["workflows"]).and_then(|o| {
            if o.code != Some(0) {
                return Err(format!("exit {:?}, stderr: {}", o.code, o.stderr.trim()));
            }
            let v: serde_json::Value =
                serde_json::from_str(&o.stdout).map_err(|e| format!("stdout is not JSON: {e}"))?;
            match v["workflows"].as_array() {
                Some(list) => Ok(format!("{} workflow(s)", list.len())),
                None => Err("no `workflows` array".into()),
            }
        }),
    );

    report
}

/// For an extension's own tests:
/// `sudohand_ext::check::assert_conformant(env!("CARGO_BIN_EXE_suh-wx"))`.
pub fn assert_conformant(path: impl AsRef<Path>) {
    let r = run(path.as_ref());
    assert!(
        r.ok,
        "{} is not a conformant extension:\n{}",
        r.path,
        r.failures()
    );
}
