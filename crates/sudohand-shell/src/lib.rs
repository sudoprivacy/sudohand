//! `sudohand-shell` — shell actuator.
//!
//! Runs one command and captures stdout / stderr / exit status, with an
//! optional timeout and output cap. [`RealShell`] is a thin wrapper over
//! `std::process::Command`; [`FakeShell`] returns canned results and
//! records every request for side-effect-free tests.
//!
//! ## RED LINE
//! **`suh serve` does not expose shell operations.** This library is
//! policy-free; other integrators choose which capabilities to link and
//! register. Exposing shell requires an explicit authorization model
//! (command allow-list, cwd fence, timeout, dry-run) distinct from
//! fs/browser/desktop.

#![deny(unsafe_code)]

pub mod backend;
pub mod fake;
pub mod real;

pub use backend::{RunRequest, RunResult, ShellBackend};
pub use fake::FakeShell;
pub use real::RealShell;
pub use sudohand_core::{Error, Result};
