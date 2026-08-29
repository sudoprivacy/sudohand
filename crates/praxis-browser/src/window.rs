//! `window_set` / `page_emulate_focus` — port of `core/window.py`.

use chromiumoxide_cdp::cdp::browser_protocol::browser::WindowState;
use chromiumoxide_cdp::cdp::browser_protocol::emulation::SetFocusEmulationEnabledParams;
use serde_json::{json, Map, Value};

use crate::config::{resolve_viewport, DEFAULT_VIEWPORT_HEIGHT, DEFAULT_VIEWPORT_WIDTH};
use crate::connection::Tab;
use crate::{Error, Result};

/// Render viewport (`width`/`height`), OS window `state`
/// (`normal`/`maximized`/`minimized`/`fullscreen`, headed only) and `focus`.
pub async fn window_set(
    tab: &Tab,
    width: Option<u32>,
    height: Option<u32>,
    state: Option<&str>,
    focus: bool,
) -> Result<Value> {
    let mut out = Map::new();
    if width.is_some() || height.is_some() {
        let vp = resolve_viewport()?.unwrap_or((DEFAULT_VIEWPORT_WIDTH, DEFAULT_VIEWPORT_HEIGHT));
        let w = width.unwrap_or(vp.0);
        let h = height.unwrap_or(vp.1);
        tab.set_viewport(w, h).await?;
        out.insert("width".into(), json!(w));
        out.insert("height".into(), json!(h));
    }
    if let Some(s) = state {
        let ws = match s {
            "maximized" => WindowState::Maximized,
            "minimized" => WindowState::Minimized,
            "fullscreen" => WindowState::Fullscreen,
            _ => WindowState::Normal,
        };
        tab.set_window_state(ws).await?;
        out.insert("state".into(), json!(s));
    }
    if focus {
        tab.activate().await?;
        out.insert("focused".into(), json!(true));
    }
    if out.is_empty() {
        return Err(Error::Invalid(
            "window_set needs at least one of width, height, state, or focus".to_string(),
        ));
    }
    Ok(Value::Object(out))
}

/// Make the page behave as if its window were focused. `{enabled}`.
pub async fn page_emulate_focus(tab: &Tab, enabled: bool) -> Result<Value> {
    tab.send(SetFocusEmulationEnabledParams::new(enabled))
        .await?;
    Ok(json!({"enabled": enabled}))
}
