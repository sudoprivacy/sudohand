//! `praxis-shell` — shell actuator.
//!
//! Runs a command and captures stdout/stderr/exit code. Thin wrapper
//! over `std::process::Command`.
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
//!
//! Placeholder until the runner lands.

/// Placeholder until the shell runner is implemented.
pub fn placeholder() -> &'static str {
    "praxis-shell: command runner goes here (gated by the integrator)"
}
