//! The OS-neutral surface: the value types agents see, the [`DesktopBackend`]
//! trait every platform implements, and the `key` grammar. No macOS types
//! appear here, so the policy/session logic in a consumer can be tested
//! against a fake backend with no Accessibility permission.

use serde::{Deserialize, Serialize};
pub use sudohand_core::Permissions;
use sudohand_core::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppInfo {
    pub bundle_id: String,
    pub name: String,
    pub pid: i32,
    pub frontmost: bool,
    pub windows: Vec<WindowInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    /// Screen coordinates, points, origin top-left.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub on_screen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AxNode {
    /// Stable within one `ax_tree` result; used by `ax_press`/`ax_set_value`/`ax_focus`.
    pub r#ref: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subrole: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    /// Screen coordinates, points, origin top-left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<[f64; 4]>,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<AxNode>,
}

/// A window capture. `png` is in **device pixels** (2× on Retina); `click`
/// takes **screen points**. Map an image pixel `(px, py)` to a click target
/// with `(origin.0 + px / scale, origin.1 + py / scale)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Screenshot {
    pub png: Vec<u8>,
    pub width_px: u32,
    pub height_px: u32,
    /// Pixels per point in `png` (after any `max_width` downscale).
    pub scale: f64,
    /// Window origin in screen points (top-left).
    pub origin: (f64, f64),
    /// The captured window's frame in screen points.
    pub window: WindowInfo,
}

/// Modifier flags for [`DesktopBackend::key`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// The OS-facing side. Everything is synchronous and may block; a consumer
/// runs it on a blocking pool. Policy, sessions, auditing and confirmation
/// prompts are NOT here — they belong to the integrator.
pub trait DesktopBackend: Send + Sync + std::fmt::Debug {
    fn permissions(&self) -> Permissions;
    /// Running apps among `bundle_ids`; with an empty slice, every running
    /// app that shows in the Dock (regular activation policy).
    fn apps(&self, bundle_ids: &[String]) -> Result<Vec<AppInfo>>;
    /// Bring `bundle_id` to the foreground (launching it if needed).
    fn activate(&self, bundle_id: &str) -> Result<()>;
    /// Capture the given window (id from [`WindowInfo`], must belong to
    /// `bundle_id`). See [`Screenshot`] for the pixel ↔ point mapping.
    fn screenshot(
        &self,
        bundle_id: &str,
        window_id: u32,
        max_width: Option<u32>,
    ) -> Result<Screenshot>;
    /// Dump the element tree of the app (optionally one window) and remember
    /// the `ref` → element mapping for later actions.
    fn ax_tree(
        &self,
        bundle_id: &str,
        window_id: Option<u32>,
        max_depth: usize,
        max_nodes: usize,
    ) -> Result<AxNode>;
    fn ax_press(&self, r#ref: &str) -> Result<()>;
    fn ax_set_value(&self, r#ref: &str, value: &str) -> Result<()>;
    fn ax_focus(&self, r#ref: &str) -> Result<()>;
    /// Click at screen coordinates. Bounds-checking against a window is the
    /// caller's responsibility.
    fn click(&self, x: f64, y: f64, button: &str, count: u32) -> Result<()>;
    /// Insert text into the focused control of the frontmost app.
    fn type_text(&self, text: &str) -> Result<()>;
    /// Paste a file (image, document, …) into the focused control of the
    /// frontmost app, the way a Finder copy + ⌘V would.
    fn paste_file(&self, path: &std::path::Path) -> Result<()>;
    fn key(&self, key: &str, mods: Modifiers) -> Result<()>;
    /// Bundle id of the frontmost application, if known.
    fn frontmost(&self) -> Option<String>;
}

/// `"cmd+shift+a"` → (`"a"`, mods). Key names are lower-cased; modifiers
/// accepted: cmd/command/meta, shift, alt/option, ctrl/control.
pub fn parse_key(spec: &str) -> Result<(String, Modifiers)> {
    let mut mods = Modifiers::default();
    let mut key = None;
    for part in spec.split('+').map(|p| p.trim().to_lowercase()) {
        match part.as_str() {
            "cmd" | "command" | "meta" => mods.cmd = true,
            "shift" => mods.shift = true,
            "alt" | "option" => mods.alt = true,
            "ctrl" | "control" => mods.ctrl = true,
            "" => return Err(Error::invalid("empty key")),
            k => {
                if key.replace(k.to_string()).is_some() {
                    return Err(Error::invalid(format!("more than one key in {spec:?}")));
                }
            }
        }
    }
    let key = key.ok_or_else(|| Error::invalid(format!("no key in {spec:?}")))?;
    Ok((key, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_grammar() {
        let (k, m) = parse_key("Cmd+Shift+A").unwrap();
        assert_eq!(k, "a");
        assert!(m.cmd && m.shift && !m.alt && !m.ctrl);
        assert_eq!(parse_key("return").unwrap().0, "return");
        assert!(parse_key("cmd+a+b").is_err());
        assert!(parse_key("cmd").is_err());
        assert!(parse_key("").is_err());
    }
}
