//! `sudohand-core` — shared foundations for the sudohand actuators
//! (browser / desktop / filesystem / shell).
//!
//! This crate holds only OS-neutral, policy-free primitives:
//! the [`Error`] model, the JSON-over-stdout CLI conventions every
//! actuator crate follows ([`print_result`]), a dependency-free base64
//! encoder ([`b64`]) and OS permission probing ([`permissions`]). It
//! contains **no** policy, sessions, auditing, confirmation prompts, or
//! transport — those belong to an integrator (e.g. apeiron-bridge) that
//! links these crates and wraps them with such concerns.
//!
//! The wire contract (inherited unchanged from adc / ai-desktop-control):
//! success prints JSON to stdout; failure prints
//! `{"error":{"kind":<category>,"message":<text>}}` to stderr and exits 1.

pub mod b64;
pub mod permissions;

pub use permissions::Permissions;
use serde::Serialize;

/// The small error type every actuator maps its failures onto. It carries
/// a category and a human message; an integrator maps each category onto
/// its own wire codes. Kept deliberately small — the OS backends only ever
/// produce these five kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A required OS permission is not granted (Accessibility, Screen
    /// Recording, …).
    PermissionDenied(String),
    /// A requested application, window, element, path, … does not exist.
    NotFound(String),
    /// A bad argument: an unknown key name, an empty key spec, and so on.
    InvalidInput(String),
    /// An OS operation failed: activation, `screencapture`, event
    /// synthesis, a locked screen, an IO error, …
    Io(String),
    /// An internal invariant failed: a CoreFoundation object could not be
    /// created, a worker thread panicked, …
    Internal(String),
}

impl Error {
    pub fn perm(m: impl Into<String>) -> Self {
        Self::PermissionDenied(m.into())
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::NotFound(m.into())
    }
    pub fn invalid(m: impl Into<String>) -> Self {
        Self::InvalidInput(m.into())
    }
    pub fn io(m: impl Into<String>) -> Self {
        Self::Io(m.into())
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::Internal(m.into())
    }
    /// Map a `std::io::Error` to the nearest category.
    pub fn from_io(err: &std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        match err.kind() {
            K::NotFound => Self::NotFound(err.to_string()),
            K::PermissionDenied => Self::PermissionDenied(err.to_string()),
            _ => Self::Io(err.to_string()),
        }
    }

    /// Stable machine category used as `kind` in the
    /// `{"error":{"kind":...}}` CLI contract.
    pub fn code(&self) -> &'static str {
        match self {
            Error::PermissionDenied(_) => "permission_denied",
            Error::NotFound(_) => "not_found",
            Error::InvalidInput(_) => "invalid_input",
            Error::Io(_) => "io",
            Error::Internal(_) => "internal",
        }
    }

    /// Process exit code by category, so a calling agent can branch on the
    /// failure without parsing stderr: 2=bad input (fix the args), 4=not
    /// found (target absent), 7=permission (grant access / sudo), 9=io /
    /// transient (safe to retry), 1=internal (a bug — do not retry).
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::InvalidInput(_) => 2,
            Error::NotFound(_) => 4,
            Error::PermissionDenied(_) => 7,
            Error::Io(_) => 9,
            Error::Internal(_) => 1,
        }
    }

    /// The human message, without the category.
    pub fn message(&self) -> &str {
        match self {
            Error::PermissionDenied(m)
            | Error::NotFound(m)
            | Error::InvalidInput(m)
            | Error::Io(m)
            | Error::Internal(m) => m,
        }
    }

    /// The `{"error":{"kind","message"}}` envelope printed to stderr on
    /// failure, as one line.
    pub fn envelope(&self) -> String {
        serde_json::json!({ "error": ErrorEnvelope { kind: self.code(), message: self.message() } })
            .to_string()
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Body of the `{"error": {...}}` envelope.
#[derive(Serialize)]
pub struct ErrorEnvelope<'a> {
    pub kind: &'a str,
    pub message: &'a str,
}

/// CLI contract shared by every sudohand actuator binary: success prints
/// pretty JSON to stdout; failure prints `{"error":{"kind","message"}}`
/// to stderr and exits 1.
pub fn print_result<T: Serialize>(r: Result<T>) -> std::process::ExitCode {
    use std::io::Write;
    match r {
        Ok(v) => match serde_json::to_string_pretty(&v) {
            Ok(json) => {
                // A closed stdout (`sudohand … | head -1`) is not our failure:
                // exit quietly instead of panicking on EPIPE.
                let _ = writeln!(std::io::stdout().lock(), "{json}");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                let e = Error::internal(format!("could not serialize result: {e}"));
                let _ = writeln!(std::io::stderr().lock(), "{}", e.envelope());
                std::process::ExitCode::from(e.exit_code())
            }
        },
        Err(e) => {
            let _ = writeln!(std::io::stderr().lock(), "{}", e.envelope());
            std::process::ExitCode::from(e.exit_code())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_and_envelope() {
        let e = Error::not_found("window 7");
        assert_eq!(e.code(), "not_found");
        assert_eq!(e.to_string(), "window 7");
        assert_eq!(
            e.envelope(),
            r#"{"error":{"kind":"not_found","message":"window 7"}}"#
        );
        assert_eq!(Error::perm("x").code(), "permission_denied");
        assert_eq!(Error::invalid("x").code(), "invalid_input");
        assert_eq!(Error::io("x").code(), "io");
        assert_eq!(Error::internal("x").code(), "internal");
    }

    #[test]
    fn from_io_maps_kinds() {
        use std::io::{Error as IoError, ErrorKind};
        assert_eq!(
            Error::from_io(&IoError::new(ErrorKind::NotFound, "nf")).code(),
            "not_found"
        );
        assert_eq!(
            Error::from_io(&IoError::new(ErrorKind::PermissionDenied, "pd")).code(),
            "permission_denied"
        );
        assert_eq!(Error::from_io(&IoError::other("o")).code(), "io");
    }
}
