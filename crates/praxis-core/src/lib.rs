//! `praxis-core` — shared foundations for the praxis actuators
//! (browser / desktop / filesystem / shell).
//!
//! This crate holds only OS-neutral, policy-free primitives:
//! the [`Error`] model, common value types, and the JSON-over-stdout
//! CLI conventions every actuator crate follows. It contains **no**
//! policy, sessions, auditing, confirmation prompts, or transport —
//! those belong to an integrator (e.g. apeiron-bridge) that links
//! these crates and wraps them with such concerns.

use serde::Serialize;

/// The small error type every actuator maps its failures onto. An
/// integrator maps these onto its own wire codes.
#[derive(Debug)]
pub enum Error {
    /// The requested target (window, element, path, process…) was not found.
    NotFound(String),
    /// A required OS permission is missing (accessibility, screen recording…).
    PermissionDenied(String),
    /// The caller passed something invalid.
    InvalidInput(String),
    /// An underlying OS / IO failure.
    Io(String),
    /// Anything not yet categorised.
    Other(String),
}

impl Error {
    pub fn not_found(m: impl Into<String>) -> Self {
        Error::NotFound(m.into())
    }
    pub fn invalid(m: impl Into<String>) -> Self {
        Error::InvalidInput(m.into())
    }
    pub fn io(m: impl Into<String>) -> Self {
        Error::Io(m.into())
    }

    /// Stable machine code used in the `{"error":{"code":...}}` CLI contract.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::PermissionDenied(_) => "permission_denied",
            Error::InvalidInput(_) => "invalid_input",
            Error::Io(_) => "io",
            Error::Other(_) => "error",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Error::NotFound(m)
            | Error::PermissionDenied(m)
            | Error::InvalidInput(m)
            | Error::Io(m)
            | Error::Other(m) => m,
        };
        write!(f, "{}: {}", self.code(), msg)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The `{"error": {...}}` envelope printed to stderr on failure.
#[derive(Serialize)]
pub struct ErrorEnvelope<'a> {
    pub code: &'a str,
    pub message: String,
}

impl Error {
    pub fn envelope(&self) -> String {
        let env = ErrorEnvelope {
            code: self.code(),
            message: self.to_string(),
        };
        serde_json::json!({ "error": env }).to_string()
    }
}

/// CLI contract shared by every praxis actuator binary:
/// success prints JSON to stdout; failure prints `{"error":{...}}`
/// to stderr and exits 1.
pub fn print_result<T: Serialize>(r: Result<T>) -> std::process::ExitCode {
    match r {
        Ok(v) => {
            println!("{}", serde_json::to_string(&v).unwrap_or_else(|_| "null".into()));
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", e.envelope());
            std::process::ExitCode::FAILURE
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
        assert!(e.envelope().contains("\"code\":\"not_found\""));
        assert!(e.envelope().contains("window 7"));
    }
}
