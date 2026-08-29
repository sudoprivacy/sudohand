//! `praxis-desktop` — desktop actuator (browser's sibling).
//!
//! Reads a running app's window list and accessibility tree, drives it
//! with synthetic mouse/keyboard events, and captures window
//! screenshots. Currently macOS-only (AXUIElement + CoreGraphics +
//! `screencapture`).
//!
//! **To be ported in from `ai-desktop-control` (adc):** `backend`
//! trait, `MacBackend`, `FakeBackend`, and the value types
//! (`AppInfo`, `WindowInfo`, `AxNode`, `Screenshot`, …). Until then
//! this is a placeholder so the workspace builds.

/// Placeholder until adc is ported in.
pub fn placeholder() -> &'static str {
    "praxis-desktop: port adc here"
}
