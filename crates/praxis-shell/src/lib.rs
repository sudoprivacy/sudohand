//! `praxis-shell` — shell actuator.
//!
//! Runs one command and captures stdout / stderr / exit status, with an
//! optional timeout and output cap. [`RealShell`] is a thin wrapper over
//! `std::process::Command`; [`FakeShell`] returns canned results and
//! records every request for side-effect-free tests.
//!
//! ## RED LINE
//! Shell is the most dangerous actuator. Per the apeiron-bridge
//! capability model, **the bridge does not expose shell to agents by
//! default.** This crate existing in the workspace does NOT mean the
//! agent gets a shell — the library is policy-free, and the "no shell
//! over the wire" decision is enforced by the integrator (which simply
//! does not link / register this crate). If shell is ever exposed, it
//! needs its own authz model (command allow-list, cwd fence, timeout,
//! dry-run) distinct from fs/browser/desktop.

#![deny(unsafe_code)]

pub mod backend;
pub mod fake;
pub mod real;

pub use backend::{RunRequest, RunResult, ShellBackend};
pub use fake::FakeShell;
pub use praxis_core::{Error, Result};
pub use real::RealShell;
