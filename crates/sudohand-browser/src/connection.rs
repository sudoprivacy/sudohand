//! Browser connection: `/json/version` → browser WebSocket → per-tab
//! WebSocket. Mirrors `core/connection.py` + the parts of `core/_tab.py`
//! the runtime subset needs.

use std::sync::Arc;
use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::browser::{self, CloseParams};
use chromiumoxide_cdp::cdp::browser_protocol::dom;
use chromiumoxide_cdp::cdp::browser_protocol::dom_storage;
use chromiumoxide_cdp::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide_cdp::cdp::browser_protocol::input;
use chromiumoxide_cdp::cdp::browser_protocol::page;
use chromiumoxide_cdp::cdp::browser_protocol::target::{
    self, ActivateTargetParams, CreateTargetParams, GetTargetsParams, TargetInfo,
};
use chromiumoxide_cdp::cdp::js_protocol::runtime;
use serde_json::Value;

use crate::cdp::{http, Connection, Event, COMMAND_TIMEOUT, MOUSE_EVENT_TIMEOUT};
use crate::config::{
    self, DialogPolicy, DEFAULT_DEBUG_HOST, DEFAULT_DEBUG_PORT, DEFAULT_PORT_RANGE,
    DESKTOP_MIN_WIDTH, TAB_URL_ENV,
};
use crate::{Error, Result};

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
/// Total budget for reaching `/json/version`. A Chrome that has just bound
/// its port can take seconds to answer while other Chromes are starting on
/// the same machine (measured: >2 s with a dozen launching concurrently).
const CONNECT_DEADLINE: Duration = Duration::from_secs(20);

/// Resolve a debug port: explicit → `AI_DEV_BROWSER_PORT` → workspace scan →
/// default. The env var short-circuits the scan entirely.
pub async fn resolve_port(port: Option<u16>) -> u16 {
    if let Some(p) = port {
        return p;
    }
    if let Some(p) = config::env_port() {
        return p;
    }
    let ws = config::current_workspace();
    if let Some(c) = crate::port::find_workspace_chromes(&ws, DEFAULT_PORT_RANGE)
        .await
        .first()
    {
        return c.port;
    }
    DEFAULT_DEBUG_PORT
}

/// Browser-level CDP client.
#[derive(Debug)]
pub struct BrowserClient {
    /// Debug host.
    pub host: String,
    /// Debug port.
    pub port: u16,
    conn: Arc<Connection>,
    targets: Vec<TargetInfo>,
}

impl BrowserClient {
    /// Connect to a running Chrome. Retries `/json/version` until
    /// [`CONNECT_DEADLINE`] elapses — a busy Chrome is slow, not absent.
    pub async fn connect(host: &str, port: u16) -> Result<Self> {
        let mut last = None;
        let mut ws_url = None;
        let deadline = tokio::time::Instant::now() + CONNECT_DEADLINE;
        loop {
            match http::ws_debugger_url(host, port, HTTP_TIMEOUT).await {
                Ok(u) => {
                    ws_url = Some(u);
                    break;
                }
                Err(e) => {
                    last = Some(e);
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
        let ws_url = ws_url.ok_or_else(|| {
            Error::Connection(format!(
                "Failed to connect to Chrome on {host}:{port}: {}",
                last.map(|e| e.to_string()).unwrap_or_default()
            ))
        })?;
        let conn = Connection::connect(&ws_url).await?;
        let mut client = Self {
            host: host.to_string(),
            port,
            conn: Arc::new(conn),
            targets: Vec::new(),
        };
        client.update_targets().await?;
        Ok(client)
    }

    /// Connect through a running local extension bridge.
    pub async fn connect_extension(port: u16) -> Result<Self> {
        let conn = Connection::connect(&format!("ws://127.0.0.1:{port}/devtools/browser")).await?;
        let status = conn
            .send_raw("_bridge.status", serde_json::json!({}))
            .await?;
        if status["extension_connected"] != true {
            return Err(Error::Connection("extension not connected; run browser_connect --transport extension for setup instructions".into()));
        }
        let mut client = Self {
            host: "127.0.0.1".into(),
            port,
            conn: Arc::new(conn),
            targets: Vec::new(),
        };
        client.update_targets().await?;
        Ok(client)
    }

    /// The browser-level connection.
    #[must_use]
    pub fn connection(&self) -> &Arc<Connection> {
        &self.conn
    }

    /// Refresh the target list.
    pub async fn update_targets(&mut self) -> Result<()> {
        let r = self.conn.send(GetTargetsParams::default()).await?;
        self.targets = r.target_infos;
        Ok(())
    }

    /// All targets (last refresh).
    #[must_use]
    pub fn targets(&self) -> &[TargetInfo] {
        &self.targets
    }

    /// Page-type targets only, in Chrome's order.
    #[must_use]
    pub fn page_targets(&self) -> Vec<&TargetInfo> {
        self.targets.iter().filter(|t| t.r#type == "page").collect()
    }

    /// Attach to `target` over its own WebSocket and enable Page + DOM.
    pub async fn tab(&self, target: &TargetInfo) -> Result<Tab> {
        let ws = format!(
            "ws://{}:{}/devtools/page/{}",
            self.host,
            self.port,
            target.target_id.inner()
        );
        let conn = Arc::new(Connection::connect(&ws).await?);
        // Best effort, like Python's _ensure_connected.
        let _ = conn.send(page::EnableParams::default()).await;
        let _ = conn.send(dom::EnableParams::default()).await;
        let tab = Tab {
            target: target.clone(),
            conn,
            browser: Arc::clone(&self.conn),
        };
        if matches!(self.host.as_str(), "127.0.0.1" | "localhost" | "::1") {
            if let Some(record) = crate::registry::lookup(self.port, self.conn.url()) {
                crate::identity::apply(&tab, &record["identity"]).await;
            }
        }
        Ok(tab)
    }

    /// Open a new tab at `url` and attach to it.
    pub async fn new_tab(&mut self, url: &str) -> Result<Tab> {
        let created = self
            .conn
            .send(
                CreateTargetParams::builder()
                    .url(url)
                    .build()
                    .map_err(Error::Invalid)?,
            )
            .await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.update_targets().await?;
        let info = self
            .targets
            .iter()
            .find(|t| t.target_id == created.target_id)
            .or_else(|| self.targets.last())
            .cloned()
            .ok_or_else(|| Error::Connection("no target after createTarget".to_string()))?;
        self.tab(&info).await
    }

    /// `Browser.close` — graceful shutdown that flushes the profile.
    pub async fn close_browser(&self) -> Result<()> {
        // Chrome may drop the socket before answering; that's success.
        match self.conn.send(CloseParams::default()).await {
            Ok(_) | Err(Error::Connection(_) | Error::Timeout { .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Connect with port auto-resolution (explicit → env → workspace scan → default).
pub async fn connect_browser(host: Option<&str>, port: Option<u16>) -> Result<BrowserClient> {
    match std::env::var("AI_DEV_BROWSER_TRANSPORT")
        .as_deref()
        .unwrap_or("cdp")
    {
        "extension" => BrowserClient::connect_extension(crate::bridge::PORT).await,
        "cdp" => {
            let host = host.unwrap_or(DEFAULT_DEBUG_HOST);
            let port = resolve_port(port).await;
            BrowserClient::connect(host, port).await
        }
        other => Err(Error::Invalid(format!(
            "Unknown browser transport: {other}"
        ))),
    }
}

/// One page target with its own CDP session.
#[derive(Debug, Clone)]
pub struct Tab {
    /// Target info as of attach time.
    pub target: TargetInfo,
    conn: Arc<Connection>,
    browser: Arc<Connection>,
}

impl Tab {
    /// The tab's connection.
    #[must_use]
    pub fn connection(&self) -> &Arc<Connection> {
        &self.conn
    }

    /// The browser-level connection this tab came from.
    #[must_use]
    pub fn browser_connection(&self) -> &Arc<Connection> {
        &self.browser
    }

    /// Send a typed command on the tab session.
    pub async fn send<C: chromiumoxide_types::Command>(&self, cmd: C) -> Result<C::Response> {
        self.conn.send(cmd).await
    }

    /// Send with a custom timeout.
    pub async fn send_timeout<C: chromiumoxide_types::Command>(
        &self,
        cmd: C,
        timeout: Duration,
    ) -> Result<C::Response> {
        self.conn.send_with(cmd, timeout, None).await
    }

    /// Subscribe to this tab's events.
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Arc<Event>> {
        self.conn.subscribe()
    }

    /// Evaluate JS with deep serialization; objects come back as JSON.
    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        self.evaluate_opts(expression, false, false).await
    }

    /// Evaluate with `awaitPromise` / `returnByValue` control.
    pub async fn evaluate_opts(
        &self,
        expression: &str,
        await_promise: bool,
        return_by_value: bool,
    ) -> Result<Value> {
        let ser = runtime::SerializationOptions {
            serialization: runtime::SerializationOptionsSerialization::Deep,
            max_depth: Some(10),
            additional_parameters: Some(
                serde_json::json!({"maxNodeDepth": 10, "includeShadowTree": "all"}),
            ),
        };
        let params = runtime::EvaluateParams {
            expression: expression.to_string(),
            object_group: None,
            include_command_line_api: None,
            silent: None,
            context_id: None,
            return_by_value: Some(return_by_value),
            generate_preview: None,
            user_gesture: Some(true),
            await_promise: Some(await_promise),
            throw_on_side_effect: None,
            timeout: None,
            disable_breaks: None,
            repl_mode: None,
            allow_unsafe_eval_blocked_by_csp: Some(true),
            unique_context_id: None,
            serialization_options: Some(ser),
            eval_as_function_fallback: None,
        };
        let r = self.send(params).await?;
        crate::js::unwrap(&r.result, r.exception_details.as_ref(), Some(expression))
            .map_err(Error::JsEvaluation)
    }

    /// Current `location.href`, or the cached target URL if the page is mid-navigation.
    pub async fn current_url(&self) -> String {
        match self.evaluate("window.location.href").await {
            Ok(Value::String(s)) => s,
            _ => self.target.url.clone(),
        }
    }

    /// `Page.navigate` then settle 0.5s (as the reference does).
    pub async fn navigate(&self, url: &str) -> Result<()> {
        let r = self
            .send(
                page::NavigateParams::builder()
                    .url(url)
                    .build()
                    .map_err(Error::Invalid)?,
            )
            .await?;
        if let Some(err) = r.error_text.filter(|e| !e.is_empty()) {
            return Err(Error::Connection(format!(
                "navigation to {url} failed: {err}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    /// Render viewport via device-metrics override.
    pub async fn set_viewport(&self, width: u32, height: u32) -> Result<()> {
        let p = SetDeviceMetricsOverrideParams::builder()
            .width(i64::from(width))
            .height(i64::from(height))
            .device_scale_factor(1.0)
            .mobile(false)
            .screen_width(i64::from(width))
            .screen_height(i64::from(height))
            .build()
            .map_err(Error::Invalid)?;
        self.send(p).await?;
        Ok(())
    }

    /// `Target.activateTarget`.
    pub async fn activate(&self) -> Result<()> {
        self.send(ActivateTargetParams::new(self.target.target_id.clone()))
            .await?;
        Ok(())
    }

    /// Capture a PNG of the viewport (or beyond it).
    pub async fn capture_png(&self, full_page: bool) -> Result<Vec<u8>> {
        let p = page::CaptureScreenshotParams {
            format: Some(page::CaptureScreenshotFormat::Png),
            quality: None,
            clip: None,
            from_surface: None,
            capture_beyond_viewport: Some(full_page),
            optimize_for_speed: None,
        };
        let r = self.send(p).await?;
        let b64: &str = r.data.as_ref();
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| Error::Image(format!("screenshot base64: {e}")))
    }

    /// `(windowId, bounds)` of the OS window hosting this tab.
    pub async fn get_window(&self) -> Result<(browser::WindowId, browser::Bounds)> {
        let r = self
            .send(browser::GetWindowForTargetParams {
                target_id: Some(self.target.target_id.clone()),
            })
            .await?;
        Ok((r.window_id, r.bounds))
    }

    /// Set the OS window state (headed Chrome only).
    pub async fn set_window_state(&self, state: browser::WindowState) -> Result<()> {
        let (id, _) = self.get_window().await?;
        let bounds = browser::Bounds {
            left: None,
            top: None,
            width: None,
            height: None,
            window_state: Some(state),
        };
        self.send(browser::SetWindowBoundsParams::new(id, bounds))
            .await?;
        Ok(())
    }

    /// Scroll by `percent` of the viewport height (negative = up) with a
    /// synthesized gesture (human-shaped acceleration), falling back to
    /// `window.scrollBy` on embedded targets that lack the Browser /
    /// synthesizeScrollGesture methods (-32601).
    pub async fn scroll_by_viewport_percent(&self, percent: f64) -> Result<()> {
        let gesture = async {
            let (_, bounds) = self.get_window().await?;
            let h = bounds.height.unwrap_or(0) as f64;
            let p = input::SynthesizeScrollGestureParams {
                x: 0.0,
                y: 0.0,
                x_distance: None,
                y_distance: Some(-(h * (percent / 100.0))),
                x_overscroll: Some(0.0),
                y_overscroll: Some(0.0),
                prevent_fling: Some(true),
                speed: Some(7777),
                gesture_source_type: None,
                repeat_count: None,
                repeat_delay_ms: Some(0),
                interaction_marker_name: None,
            };
            self.send(p).await?;
            Ok::<(), Error>(())
        };
        match gesture.await {
            Ok(()) => Ok(()),
            Err(e) if is_method_not_found(&e) => {
                self.evaluate(&format!(
                    "window.scrollBy(0, window.innerHeight * {})",
                    percent / 100.0
                ))
                .await?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// `Page.reload`.
    pub async fn reload(&self, ignore_cache: bool) -> Result<()> {
        self.send(page::ReloadParams {
            ignore_cache: Some(ignore_cache),
            script_to_evaluate_on_load: None,
            loader_id: None,
        })
        .await?;
        Ok(())
    }

    /// `window.location.origin` (falls back to the target URL's origin).
    pub async fn origin(&self) -> String {
        if let Ok(Value::String(o)) = self.evaluate("window.location.origin").await {
            if !o.is_empty() && o != "null" {
                return o;
            }
        }
        let u = &self.target.url;
        u.splitn(4, '/').take(3).collect::<Vec<_>>().join("/")
    }

    /// All `localStorage` items for the page origin.
    pub async fn storage_get(&self) -> Result<serde_json::Map<String, Value>> {
        let origin = self.origin().await;
        let r = self
            .send(dom_storage::GetDomStorageItemsParams {
                storage_id: dom_storage::StorageId {
                    security_origin: Some(origin),
                    storage_key: None,
                    is_local_storage: true,
                },
            })
            .await?;
        let mut out = serde_json::Map::new();
        for item in r.entries {
            let kv = item.inner();
            if let (Some(k), Some(v)) = (kv.first(), kv.get(1)) {
                out.insert(k.clone(), Value::String(v.clone()));
            }
        }
        Ok(out)
    }

    /// Set `localStorage` items for the page origin.
    pub async fn storage_set(&self, items: &serde_json::Map<String, Value>) -> Result<()> {
        let origin = self.origin().await;
        for (k, v) in items {
            let value = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            self.send(dom_storage::SetDomStorageItemParams {
                storage_id: dom_storage::StorageId {
                    security_origin: Some(origin.clone()),
                    storage_key: None,
                    is_local_storage: true,
                },
                key: k.clone(),
                value,
            })
            .await?;
        }
        Ok(())
    }

    /// Resolve a cross-origin iframe (OOPIF) to a flat-session id, attaching
    /// once. `frame` is a URL substring or an exact target id.
    pub async fn frame_session(&self, frame: &str) -> Result<String> {
        let targets = self.send(GetTargetsParams::default()).await?.target_infos;
        let iframes: Vec<&TargetInfo> = targets.iter().filter(|t| t.r#type == "iframe").collect();
        let Some(m) = iframes
            .iter()
            .find(|t| t.target_id.inner() == frame || t.url.contains(frame))
        else {
            let available: Vec<&str> = iframes
                .iter()
                .filter(|t| !t.url.is_empty())
                .map(|t| t.url.as_str())
                .collect();
            let available = if available.is_empty() {
                "[\"(none)\"]".to_string()
            } else {
                format!("{available:?}")
            };
            return Err(Error::Invalid(format!(
                "no cross-origin iframe target matching {frame:?}. cross-origin iframes on this page: {available} (pass any substring; same-origin iframes don't need --frame)"
            )));
        };
        let r = self
            .send(target::AttachToTargetParams {
                target_id: m.target_id.clone(),
                flatten: Some(true),
            })
            .await?;
        Ok(r.session_id.inner().clone())
    }

    /// Evaluate inside a flat session (a cross-origin iframe).
    pub async fn evaluate_in_session(&self, expression: &str, session_id: &str) -> Result<Value> {
        let ser = runtime::SerializationOptions {
            serialization: runtime::SerializationOptionsSerialization::Deep,
            max_depth: Some(10),
            additional_parameters: Some(
                serde_json::json!({"maxNodeDepth": 10, "includeShadowTree": "all"}),
            ),
        };
        let params = runtime::EvaluateParams {
            expression: expression.to_string(),
            object_group: None,
            include_command_line_api: None,
            silent: None,
            context_id: None,
            return_by_value: Some(false),
            generate_preview: None,
            user_gesture: Some(true),
            await_promise: Some(false),
            throw_on_side_effect: None,
            timeout: None,
            disable_breaks: None,
            repl_mode: None,
            allow_unsafe_eval_blocked_by_csp: Some(true),
            unique_context_id: None,
            serialization_options: Some(ser),
            eval_as_function_fallback: None,
        };
        let r = self
            .conn
            .send_with(params, COMMAND_TIMEOUT, Some(session_id))
            .await?;
        crate::js::unwrap(&r.result, r.exception_details.as_ref(), Some(expression))
            .map_err(Error::JsEvaluation)
    }

    /// Close this tab's target.
    pub async fn close_target(&self) -> Result<()> {
        self.browser
            .send(target::CloseTargetParams::new(
                self.target.target_id.clone(),
            ))
            .await?;
        Ok(())
    }

    /// Press+release at `(x, y)`.
    pub async fn mouse_click(&self, x: f64, y: f64) -> Result<()> {
        self.mouse_click_with(x, y, input::MouseButton::Left, 0)
            .await
    }

    /// Press+release at `(x, y)` with a button and modifier bitmask
    /// (1=Alt, 2=Ctrl, 4=Meta, 8=Shift).
    pub async fn mouse_click_with(
        &self,
        x: f64,
        y: f64,
        button: input::MouseButton,
        modifiers: i64,
    ) -> Result<()> {
        for t in [
            input::DispatchMouseEventType::MousePressed,
            input::DispatchMouseEventType::MouseReleased,
        ] {
            let p = input::DispatchMouseEventParams::builder()
                .r#type(t)
                .x(x)
                .y(y)
                .button(button.clone())
                .click_count(1)
                .modifiers(modifiers)
                .build()
                .map_err(Error::Invalid)?;
            self.send_timeout(p, MOUSE_EVENT_TIMEOUT).await?;
        }
        Ok(())
    }

    /// Linear move from `(0, 0)` to `(x, y)` in `steps` `mouseMoved` events
    /// (Python `Tab.mouse_move`; `steps <= 1` is a single event).
    pub async fn mouse_move_steps(&self, x: f64, y: f64, steps: usize) -> Result<()> {
        if steps <= 1 {
            return self.mouse_move(x, y).await;
        }
        for i in 0..steps {
            let t = (i + 1) as f64 / steps as f64;
            self.mouse_move(x * t, y * t).await?;
        }
        Ok(())
    }

    /// Left-button drag from `from` to `to` along a straight line with
    /// `steps` intermediate moves (Python `Tab.mouse_drag`).
    pub async fn mouse_drag(&self, from: (f64, f64), to: (f64, f64), steps: usize) -> Result<()> {
        let press = input::DispatchMouseEventParams::builder()
            .r#type(input::DispatchMouseEventType::MousePressed)
            .x(from.0)
            .y(from.1)
            .button(input::MouseButton::Left)
            .click_count(1)
            .build()
            .map_err(Error::Invalid)?;
        self.send_timeout(press, MOUSE_EVENT_TIMEOUT).await?;
        for i in 0..steps.max(1) {
            let t = (i + 1) as f64 / steps.max(1) as f64;
            self.mouse_move(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
                .await?;
        }
        let release = input::DispatchMouseEventParams::builder()
            .r#type(input::DispatchMouseEventType::MouseReleased)
            .x(to.0)
            .y(to.1)
            .button(input::MouseButton::Left)
            .click_count(1)
            .build()
            .map_err(Error::Invalid)?;
        self.send_timeout(release, MOUSE_EVENT_TIMEOUT).await?;
        Ok(())
    }

    /// Move the cursor to `(x, y)`.
    pub async fn mouse_move(&self, x: f64, y: f64) -> Result<()> {
        let p = input::DispatchMouseEventParams::builder()
            .r#type(input::DispatchMouseEventType::MouseMoved)
            .x(x)
            .y(y)
            .build()
            .map_err(Error::Invalid)?;
        self.send_timeout(p, MOUSE_EVENT_TIMEOUT).await?;
        Ok(())
    }
}

/// CDP `-32601` (method not found): an embedded target (Electron, CEF)
/// without the Browser / gesture domains.
fn is_method_not_found(e: &Error) -> bool {
    let t = e.to_string();
    t.contains("-32601") || t.contains("wasn't found")
}

fn path_part(u: &str) -> &str {
    let u = u.split('#').next().unwrap_or(u);
    u.split('?').next().unwrap_or(u)
}

async fn prepare(tab: Tab) -> Result<Tab> {
    // Dialogs first: an open alert blocks the renderer, so the viewport
    // probe below would hang.
    if let Some(policy) = config::resolve_dialog_policy() {
        let accept = policy == DialogPolicy::Accept;
        let conn = Arc::clone(&tab.conn);
        let mut rx = conn.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Page.javascriptDialogOpening" {
                    let _ = conn
                        .send(page::HandleJavaScriptDialogParams::new(accept))
                        .await;
                }
            }
        });
        let _ = tab
            .send(page::HandleJavaScriptDialogParams::new(accept))
            .await;
    }
    let Some((w, h)) = config::resolve_viewport()? else {
        return Ok(tab);
    };
    let current = tab
        .evaluate("window.innerWidth")
        .await
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if current < DESKTOP_MIN_WIDTH {
        tab.set_viewport(w, h).await?;
    }
    Ok(tab)
}

/// Pick the tab to act on. There is no "active tab" in CDP: without
/// `url_contains` (or `AI_DEV_BROWSER_TAB_URL`) it's the first page target
/// whose URL isn't `about:*`.
pub async fn get_active_tab(
    browser: &mut BrowserClient,
    url_contains: Option<&str>,
) -> Result<Tab> {
    let wanted = url_contains
        .map(str::to_string)
        .or_else(|| std::env::var(TAB_URL_ENV).ok())
        .unwrap_or_default();
    let pages: Vec<TargetInfo> = browser.page_targets().into_iter().cloned().collect();
    if !wanted.is_empty() {
        if let Some(t) = pages.iter().find(|t| path_part(&t.url).contains(&wanted)) {
            return prepare(browser.tab(t).await?).await;
        }
        if let Some(t) = pages.iter().find(|t| t.url.contains(&wanted)) {
            return prepare(browser.tab(t).await?).await;
        }
        let urls: Vec<&str> = pages.iter().map(|t| t.url.as_str()).collect();
        return Err(Error::Invalid(format!(
            "No tab whose URL contains {wanted:?}. Open page targets: {}",
            if urls.is_empty() {
                "none".to_string()
            } else {
                format!("{urls:?}")
            }
        )));
    }
    if let Some(t) = pages
        .iter()
        .find(|t| !t.url.is_empty() && !t.url.starts_with("about:"))
    {
        return prepare(browser.tab(t).await?).await;
    }
    if let Some(t) = pages.first() {
        return prepare(browser.tab(t).await?).await;
    }
    let tab = browser.new_tab("about:blank").await?;
    prepare(tab).await
}
