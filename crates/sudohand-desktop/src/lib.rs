//! `sudohand-desktop` — the desktop actuator (browser's sibling), ported
//! from **ai-desktop-control** (`adc`): read a running app's window list
//! and Accessibility tree, drive it with synthetic mouse/keyboard events,
//! and capture window screenshots.
//!
//! It is a library; the CLI lives in `sudohand-cli` (`sudohand desktop <cmd>`).
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
//! - [`vlm`] / [`workflow`] / [`flow`] / [`registry`] (feature `agent`, off
//!   by default) — VLM-assisted element location, the workflow step DSL, its
//!   execution as a graph-flow graph (scripted positions first, VLM
//!   fallback), and the name → workflow registry. Desktop-only by design:
//!   flows live inside one actuator; there is no cross-actuator workflow.
//!
//! The error model ([`Error`] / [`Result`]), base64 and permission probing
//! come from `sudohand-core`.

#![deny(unsafe_code)]

pub mod backend;
pub mod fake;
#[cfg(feature = "agent")]
pub mod flow;
#[cfg(feature = "agent")]
pub mod registry;
#[cfg(feature = "agent")]
pub mod vlm;
#[cfg(feature = "agent")]
pub mod workflow;

#[cfg(target_os = "macos")]
pub mod macos;

#[doc(hidden)]
pub use sudohand_core::b64;
pub use sudohand_core::{Error, Permissions, Result};

pub use backend::{parse_key, AppInfo, AxNode, DesktopBackend, Modifiers, Screenshot, WindowInfo};
pub use fake::FakeBackend;

#[cfg(target_os = "macos")]
pub use macos::MacBackend;
