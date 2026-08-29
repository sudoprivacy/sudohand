//! `praxis-desktop` — the desktop actuator (browser's sibling), ported
//! from **ai-desktop-control** (`adc`): read a running app's window list
//! and Accessibility tree, drive it with synthetic mouse/keyboard events,
//! and capture window screenshots.
//!
//! It is a library; the CLI lives in `praxis-cli` (`praxis desktop <cmd>`).
//! It deliberately contains no policy, sessions, auditing, confirmation
//! prompts, or transport — an integrator such as `apeiron-bridge` wraps the
//! [`DesktopBackend`] with those and exposes it over the wire.
//!
//! - [`backend`] — OS-neutral value types, the [`DesktopBackend`] trait, and
//!   the `key` grammar ([`parse_key`]).
//! - [`macos`] — the real backend ([`MacBackend`]): AXUIElement + CoreGraphics
//!   synthetic events + `screencapture`. The one module that uses `unsafe`.
//! - [`fake`] — a recording backend ([`FakeBackend`]) for permission-free tests.
//!
//! The error model ([`Error`] / [`Result`]), base64 and permission probing
//! come from `praxis-core`. adc's agent layer (VLM locate, workflows,
//! graph-flow) is intentionally **not** here — it belongs to a future flow /
//! integration crate, not the thin execution surface.

#![deny(unsafe_code)]

pub mod backend;
pub mod fake;

#[cfg(target_os = "macos")]
pub mod macos;

#[doc(hidden)]
pub use praxis_core::b64;
pub use praxis_core::{Error, Permissions, Result};

pub use backend::{parse_key, AppInfo, AxNode, DesktopBackend, Modifiers, Screenshot, WindowInfo};
pub use fake::FakeBackend;

#[cfg(target_os = "macos")]
pub use macos::MacBackend;
