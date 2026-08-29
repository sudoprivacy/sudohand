//! How a workflow step actually runs an action. A [`Dispatch`] takes an
//! argv — `["desktop", "locate", "--find", "Log out"]`, i.e. a `suh`
//! subcommand without the `suh` — and returns the action's result JSON (or
//! a mapped error). The default [`CliDispatch`] execs `$SUH_BIN`; tests use
//! a fake.

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use sudohand_core::{Error, Result};

/// Runs one action and returns its JSON result. Actuator-agnostic: the
/// engine never links fs/shell/browser/desktop — it speaks the CLI contract.
pub trait Dispatch: Send + Sync {
    fn call(&self, argv: &[String]) -> Result<Value>;
}

/// Execs `suh <argv…>` (JSON on stdout; `{"error":{kind,message}}` on
/// stderr, exit 1). The binary is `SUH_BIN` if set, else `suh` on `PATH`.
pub struct CliDispatch {
    pub bin: PathBuf,
}

impl Default for CliDispatch {
    fn default() -> Self {
        Self {
            bin: std::env::var_os("SUH_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("suh")),
        }
    }
}

impl Dispatch for CliDispatch {
    fn call(&self, argv: &[String]) -> Result<Value> {
        let out = Command::new(&self.bin)
            .args(argv)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                Error::io(format!(
                    "run {} {}: {e}",
                    self.bin.display(),
                    argv.join(" ")
                ))
            })?;
        if out.status.success() {
            if out.stdout.is_empty() {
                return Ok(Value::Null);
            }
            serde_json::from_slice(&out.stdout)
                .map_err(|e| Error::internal(format!("action stdout not JSON: {e}")))
        } else {
            Err(parse_error(&out.stderr))
        }
    }
}

/// Turn a `{"error":{kind,message}}` line back into an [`Error`].
fn parse_error(stderr: &[u8]) -> Error {
    let text = String::from_utf8_lossy(stderr);
    let line = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(line) {
        let kind = v["error"]["kind"].as_str().unwrap_or("");
        let msg = v["error"]["message"].as_str().unwrap_or(line).to_string();
        return match kind {
            "permission_denied" => Error::perm(msg),
            "not_found" => Error::not_found(msg),
            "invalid_input" => Error::invalid(msg),
            "internal" => Error::internal(msg),
            _ => Error::io(msg),
        };
    }
    Error::io(if line.is_empty() {
        "action failed".into()
    } else {
        line.to_string()
    })
}
