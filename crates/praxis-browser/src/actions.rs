//! Element actions by `ref`: `click_by_ref`, `click_by_text`, `type_by_ref`,
//! `focus_by_ref`, `hover_by_ref`, `highlight_by_ref`, `html_by_ref`,
//! `screenshot_by_ref`, `select_by_ref`, `upload_by_ref`, `drag_by_ref`,
//! `press_key`. Port of `core/ax.py`.
//!
//! Every element click goes through the shared human-like actuator
//! ([`crate::human::click_box`]: in-bounds random offset, optional gaussian
//! path / hold time) unless `human_like` is false, in which case it is a
//! bare `mousePressed` → `mouseReleased` at the content-box centre — the
//! same two paths as `ax._click_by_node_id`. Both run `scrollIntoViewIfNeeded`
//! first.

use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, FocusParams, ResolveNodeParams, ScrollIntoViewIfNeededParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, InsertTextParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::GetFrameTreeParams;
use chromiumoxide_cdp::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use serde_json::{json, Map, Value};

use crate::cdp::MOUSE_EVENT_TIMEOUT;
use crate::connection::Tab;
use crate::element::{find_element_by_text, DomElement};
use crate::human;
use crate::refs::{frame_scope, node_id_of, parse_ref};
use crate::snapshot::{box_model_quad, get_snapshot, page_discover, DiscoverOptions, Element};
use crate::text_match::best_match;
use crate::{Error, Result};

/// Delay after an action before reading the post-action URL.
pub(crate) const POST_CLICK_NAV_DELAY: Duration = Duration::from_millis(300);
pub(crate) const AX_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// `{url, title}` of the top frame.
pub(crate) async fn capture_page_state(tab: &Tab) -> Result<(String, String)> {
    let v = tab
        .evaluate("({url: window.location.href, title: document.title})")
        .await?;
    Ok((
        v.get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        v.get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    ))
}

/// Attach `url_after` / `title_after` / `navigated` to an action result.
pub(crate) async fn with_nav_feedback(
    tab: &Tab,
    mut result: Map<String, Value>,
) -> Map<String, Value> {
    let url_before = result
        .get("url_before")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    tokio::time::sleep(POST_CLICK_NAV_DELAY).await;
    if let Ok((url, title)) = capture_page_state(tab).await {
        let navigated = !url_before.is_empty() && url != url_before;
        result.insert("url_after".into(), json!(url));
        result.insert("title_after".into(), json!(title));
        result.insert("navigated".into(), json!(navigated));
    } else {
        // Context destroyed mid-read (full-page nav) — it navigated.
        result.insert("navigated".into(), json!(true));
        result.insert("url_after".into(), Value::Null);
        result.insert("title_after".into(), Value::Null);
    }
    result
}

pub(crate) fn failed(mut result: Map<String, Value>, url_before: &str) -> Map<String, Value> {
    result.insert("url_before".into(), json!(url_before));
    result.insert("navigated".into(), json!(false));
    result.insert("url_after".into(), json!(url_before));
    result.insert("title_after".into(), json!(""));
    result
}

/// Centre of a content quad (average of its four corners).
#[must_use]
pub fn quad_center(q: &[f64]) -> (f64, f64) {
    (
        (q[0] + q[2] + q[4] + q[6]) / 4.0,
        (q[1] + q[3] + q[5] + q[7]) / 4.0,
    )
}

/// Width and height of a content quad (`DOM.getBoxModel` reports them
/// separately; deriving them from the quad keeps one source of truth).
#[must_use]
pub fn quad_size(q: &[f64]) -> (f64, f64) {
    let w = ((q[2] - q[0]).powi(2) + (q[3] - q[1]).powi(2)).sqrt();
    let h = ((q[6] - q[0]).powi(2) + (q[7] - q[1]).powi(2)).sqrt();
    (w, h)
}

/// Actionable message for a ref whose node Chrome no longer knows.
#[must_use]
pub fn stale_ref_message(r#ref: &str) -> String {
    format!(
        "stale or unknown ref '{ref}' — the node is not in the current page snapshot \
         (refs are only valid within one page_discover snapshot; they change after a \
         navigation, a reload, or in a new session); run page_discover again to get fresh refs"
    )
}

/// Check that `node_id` still exists in the page. `Err` carries the
/// actionable stale-ref message — this is what separates "the ref is dead"
/// from "the node exists but has no geometry".
pub async fn resolve_ref_node(tab: &Tab, r#ref: &str, node_id: i64) -> Result<()> {
    let p = DescribeNodeParams {
        backend_node_id: Some(BackendNodeId::new(node_id)),
        depth: Some(0),
        ..Default::default()
    };
    match tab.send(p).await {
        Ok(_) => {}
        Err(Error::Protocol { .. }) => return Err(Error::Invalid(stale_ref_message(r#ref))),
        Err(e) => return Err(e),
    }
    // Chrome keeps a navigated-away document's nodes describable for a
    // while, so "known id" is not "in the current page". A node from a
    // detached document has `ownerDocument.defaultView === null`.
    let rp = ResolveNodeParams {
        backend_node_id: Some(BackendNodeId::new(node_id)),
        ..Default::default()
    };
    let obj = match tab.send(rp).await {
        Ok(r) => r.object,
        Err(Error::Protocol { .. }) => return Err(Error::Invalid(stale_ref_message(r#ref))),
        Err(e) => return Err(e),
    };
    let Some(oid) = obj.object_id else {
        return Err(Error::Invalid(stale_ref_message(r#ref)));
    };
    let call = CallFunctionOnParams::builder()
        .function_declaration(
            "function() { return !!(this.isConnected && this.ownerDocument && this.ownerDocument.defaultView); }",
        )
        .object_id(oid)
        .return_by_value(true)
        .build()
        .map_err(Error::Invalid)?;
    match tab.send(call).await {
        Ok(r) if r.result.value == Some(Value::Bool(true)) => Ok(()),
        Ok(_) | Err(Error::Protocol { .. }) => Err(Error::Invalid(stale_ref_message(r#ref))),
        Err(e) => Err(e),
    }
}

/// Click a backend node at its content-box centre. `ref` is only used for
/// the error message when the node has gone away.
pub async fn click_by_node_id(
    tab: &Tab,
    node_id: i64,
    r#ref: Option<&str>,
    human_like: bool,
) -> Map<String, Value> {
    let mut out = Map::new();
    if let Err(e) = resolve_ref_node(tab, r#ref.unwrap_or(""), node_id).await {
        out.insert("clicked".into(), json!(false));
        out.insert("error".into(), json!(e.to_string()));
        return out;
    }
    let scroll = ScrollIntoViewIfNeededParams {
        backend_node_id: Some(BackendNodeId::new(node_id)),
        ..Default::default()
    };
    let _ = tab.send(scroll).await;
    let Some(quad) = box_model_quad(tab, node_id).await else {
        out.insert("clicked".into(), json!(false));
        out.insert(
            "error".into(),
            json!(format!(
                "element for ref '{}' exists but is not rendered (no box model: display:none, \
                 detached, or zero-size); it cannot be clicked at coordinates",
                r#ref.unwrap_or("")
            )),
        );
        return out;
    };
    let centre = quad_center(&quad);
    let (width, height) = quad_size(&quad);
    let clicked = if human_like {
        human::click_box(tab, centre, width, height, None, None).await
    } else {
        tab.mouse_click(centre.0, centre.1).await
    };
    match clicked {
        Ok(()) => {
            out.insert("clicked".into(), json!(true));
            out.insert("node_id".into(), json!(node_id));
        }
        Err(e) => {
            out.insert("clicked".into(), json!(false));
            out.insert("error".into(), json!(e.to_string()));
        }
    }
    out
}

async fn frame_id_by_prefix(tab: &Tab, prefix: &crate::refs::ScopePrefix) -> Option<String> {
    fn find(
        t: &chromiumoxide_cdp::cdp::browser_protocol::page::FrameTree,
        prefix: &crate::refs::ScopePrefix,
    ) -> Option<String> {
        let id = t.frame.id.inner().clone();
        if &frame_scope(&id) == prefix {
            return Some(id);
        }
        t.child_frames
            .as_ref()?
            .iter()
            .find_map(|c| find(c, prefix))
    }
    let r = tab.send(GetFrameTreeParams::default()).await.ok()?;
    find(&r.frame_tree, prefix)
}

/// Click by `ref` (with embedded node id, or legacy index-only) and report
/// navigation feedback: `{clicked, ref, url_before, url_after, title_after, navigated}`.
pub async fn click_by_ref(tab: &Tab, r#ref: &str, human_like: bool) -> Result<Value> {
    let (url_before, _) = capture_page_state(tab).await?;
    let parsed = parse_ref(r#ref);

    let mut result = if let Some(node_id) = parsed.backend_node_id {
        let mut r = click_by_node_id(tab, node_id, Some(r#ref), human_like).await;
        if r.get("clicked") == Some(&json!(true)) {
            r.insert("ref".into(), json!(r#ref));
        }
        r
    } else {
        // Legacy: re-snapshot and find by index.
        let frame_id = match &parsed.scope {
            Some(p) => {
                if let Some(f) = frame_id_by_prefix(tab, p).await {
                    Some(f)
                } else {
                    let mut m = Map::new();
                    m.insert("error".into(), json!(format!("Frame '{p}' not found")));
                    return Ok(Value::Object(failed(m, &url_before)));
                }
            }
            None => None,
        };
        let elements = get_snapshot(tab, false, frame_id.as_deref(), true).await?;
        let target = elements
            .iter()
            .find(|e| parse_ref(&e.r#ref).local == parsed.local);
        let Some(target) = target else {
            let mut m = Map::new();
            m.insert(
                "error".into(),
                json!(format!("Element with ref '{}' not found", r#ref)),
            );
            return Ok(Value::Object(failed(m, &url_before)));
        };
        let Some(nid) = node_id_of(&target.r#ref) else {
            let mut m = Map::new();
            m.insert(
                "error".into(),
                json!(format!("Element ref '{}' has no nodeId", r#ref)),
            );
            return Ok(Value::Object(failed(m, &url_before)));
        };
        let mut r = click_by_node_id(tab, nid, Some(r#ref), human_like).await;
        if r.get("clicked") == Some(&json!(true)) {
            r.insert("ref".into(), json!(r#ref));
            r.insert(
                "element".into(),
                json!({"role": target.role, "name": target.name}),
            );
        }
        r
    };

    if result.get("clicked") != Some(&json!(true)) {
        return Ok(Value::Object(failed(result, &url_before)));
    }
    result.insert("url_before".into(), json!(url_before));
    Ok(Value::Object(with_nav_feedback(tab, result).await))
}

fn best_named(query: &str, candidates: Vec<Element>) -> Option<Element> {
    if candidates.is_empty() {
        return None;
    }
    let names: Vec<String> = candidates
        .iter()
        .map(|c| c.name.clone().unwrap_or_default())
        .collect();
    let idx = best_match(query, &names, 0.4, false).map_or(0, |m| m.index);
    candidates.into_iter().nth(idx)
}

/// Locate by accessible name: interactable tier first, then (unless
/// `interactable_only`) any node. One shot, no waiting — the single answer
/// to "where is the element labelled X" shared by every `*_by_text` tool.
pub async fn ax_by_text(tab: &Tab, text: &str, interactable_only: bool) -> Result<Option<Element>> {
    let mut opts = DiscoverOptions {
        text: Some(text.to_string()),
        ..DiscoverOptions::default()
    };
    let hits = page_discover(tab, &opts).await?;
    if !hits.is_empty() {
        return Ok(best_named(text, hits));
    }
    if interactable_only {
        return Ok(None);
    }
    opts.interactable_only = false;
    let hits = page_discover(tab, &opts).await?;
    Ok(best_named(text, hits))
}

/// [`ax_by_text`] retried every 0.3 s until `timeout` seconds.
pub async fn wait_ax_by_text(tab: &Tab, text: &str, timeout: f64) -> Result<Option<Element>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    loop {
        if let Some(hit) = ax_by_text(tab, text, false).await? {
            return Ok(Some(hit));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(AX_POLL_INTERVAL).await;
    }
}

/// Locate by visible text / accessible name and click it. Waits up to
/// `timeout` seconds for the text to appear.
pub async fn click_by_text(tab: &Tab, text: &str, timeout: f64, human_like: bool) -> Result<Value> {
    let (url_before, _) = capture_page_state(tab).await?;
    // Tier 1 — accessible name, the locator `find_by_text` uses.
    if let Some(el) = wait_ax_by_text(tab, text, timeout).await? {
        let mut r = click_by_ref(tab, &el.r#ref, human_like).await?;
        if let Some(m) = r.as_object_mut() {
            m.insert("text".into(), json!(text));
        }
        return Ok(r);
    }
    // Tier 2 — DOM text-node search (single text node, top frame): text in
    // an attribute, or nodes Chrome omits from the AX tree.
    let clicked = match find_element_by_text(tab, text, true).await? {
        Some(hit) => {
            if human_like {
                human::click_element(tab, &hit.element).await?;
            } else {
                hit.element.click_js(tab).await?;
            }
            true
        }
        None => false,
    };
    let mut m = Map::new();
    m.insert("clicked".into(), json!(clicked));
    m.insert("text".into(), json!(text));
    if !clicked {
        return Ok(Value::Object(failed(m, &url_before)));
    }
    m.insert("url_before".into(), json!(url_before));
    Ok(Value::Object(with_nav_feedback(tab, m).await))
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// `(dom key, dom code, virtual key code, text)`.
type KeySpec = (&'static str, &'static str, i64, Option<&'static str>);

pub(crate) fn key_spec(name: &str) -> Option<KeySpec> {
    let norm: String = name
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .collect();
    let norm = match norm.as_str() {
        "esc" => "escape",
        "del" => "delete",
        "return" => "enter",
        "up" => "arrowup",
        "down" => "arrowdown",
        "left" => "arrowleft",
        "right" => "arrowright",
        other => other,
    };
    Some(match norm {
        "enter" => ("Enter", "Enter", 13, Some("\r")),
        "tab" => ("Tab", "Tab", 9, None),
        "escape" => ("Escape", "Escape", 27, None),
        "backspace" => ("Backspace", "Backspace", 8, None),
        "delete" => ("Delete", "Delete", 46, None),
        "space" => (" ", "Space", 32, Some(" ")),
        "arrowup" => ("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" => ("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" => ("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" => ("ArrowRight", "ArrowRight", 39, None),
        "home" => ("Home", "Home", 36, None),
        "end" => ("End", "End", 35, None),
        "pageup" => ("PageUp", "PageUp", 33, None),
        "pagedown" => ("PageDown", "PageDown", 34, None),
        _ => return None,
    })
}

/// The one key-dispatch path: real keyDown + keyUp with `windowsVirtualKeyCode`.
pub async fn dispatch_key(
    tab: &Tab,
    key: &str,
    code: &str,
    vkey: i64,
    modifiers: i64,
    text: Option<&str>,
) -> Result<()> {
    let mut down = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyDown)
        .key(key)
        .code(code)
        .windows_virtual_key_code(vkey)
        .native_virtual_key_code(vkey)
        .modifiers(modifiers);
    if let Some(t) = text {
        down = down.text(t).unmodified_text(t);
    }
    tab.send_timeout(down.build().map_err(Error::Invalid)?, MOUSE_EVENT_TIMEOUT)
        .await?;
    let up = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(key)
        .code(code)
        .windows_virtual_key_code(vkey)
        .native_virtual_key_code(vkey)
        .modifiers(modifiers)
        .build()
        .map_err(Error::Invalid)?;
    tab.send_timeout(up, MOUSE_EVENT_TIMEOUT).await?;
    Ok(())
}

async fn focus_node(tab: &Tab, r#ref: &str) -> Result<i64> {
    let node_id =
        node_id_of(r#ref).ok_or_else(|| Error::Invalid(format!("Invalid ref format: {ref}")))?;
    resolve_ref_node(tab, r#ref, node_id).await?;
    let p = FocusParams {
        backend_node_id: Some(BackendNodeId::new(node_id)),
        ..Default::default()
    };
    tab.send(p).await?;
    Ok(node_id)
}

/// Options for [`type_by_ref`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TypeOptions {
    /// Clear existing content first (select + real Backspace).
    pub clear: bool,
    /// Press Enter after typing.
    pub enter: bool,
    /// Per-character key events instead of one `insertText` commit.
    pub keystrokes: bool,
    /// Character-by-character with human timing (`keystrokes` wins if both).
    pub human_like: bool,
}

/// Type into the element a `ref` names. Returns `{typed, ref, text}`
/// (+ `entered` when `enter`).
pub async fn type_by_ref(tab: &Tab, r#ref: &str, text: &str, opts: TypeOptions) -> Result<Value> {
    let node_id = match focus_node(tab, r#ref).await {
        Ok(n) => n,
        Err(e) => {
            return Ok(json!({"typed": false, "error": e.to_string()}));
        }
    };
    if opts.clear {
        let select = async {
            let rp = ResolveNodeParams {
                backend_node_id: Some(BackendNodeId::new(node_id)),
                ..Default::default()
            };
            let obj = tab.send(rp).await?.object;
            let Some(oid) = obj.object_id else {
                return Ok::<(), Error>(());
            };
            let mut call = CallFunctionOnParams::builder()
                .function_declaration(
                    "(el) => { if (el.focus) el.focus(); if (typeof el.select === 'function') { el.select(); } else { const r = document.createRange(); r.selectNodeContents(el); const s = window.getSelection(); s.removeAllRanges(); s.addRange(r); } }",
                )
                .object_id(oid.clone())
                .return_by_value(true)
                .user_gesture(true)
                .build()
                .map_err(Error::Invalid)?;
            call.arguments = Some(vec![CallArgument::builder().object_id(oid).build()]);
            tab.send(call).await?;
            Ok(())
        };
        let _ = select.await;
        let (k, c, v, _) = key_spec("backspace").expect("known key");
        dispatch_key(tab, k, c, v, 0, None).await?;
    }
    if opts.keystrokes {
        for ch in text.chars() {
            let s = ch.to_string();
            dispatch_key(tab, &s, "", 0, 0, Some(&s)).await?;
        }
    } else if opts.human_like {
        human::type_text(tab, text, Some(true)).await?;
    } else {
        tab.send(InsertTextParams::new(text)).await?;
    }
    let mut out = Map::new();
    out.insert("typed".into(), json!(true));
    out.insert("ref".into(), json!(r#ref));
    out.insert("text".into(), json!(text));
    if opts.enter {
        let pressed = focus_node(tab, r#ref).await.is_ok() && {
            let (k, c, v, t) = key_spec("enter").expect("known key");
            dispatch_key(tab, k, c, v, 0, t).await.is_ok()
        };
        out.insert("entered".into(), json!(pressed));
    }
    Ok(Value::Object(out))
}

/// Drag the element `ref` names to `(to_x, to_y)`: scroll it into view,
/// press at its content-box centre, move in `steps`, release. Returns
/// `{dragged, ref}`. `human_like` uses the gaussian drag (Rust extension).
pub async fn drag_by_ref(
    tab: &Tab,
    r#ref: &str,
    to_x: f64,
    to_y: f64,
    steps: usize,
    human_like: bool,
) -> Result<Value> {
    let node_id =
        node_id_of(r#ref).ok_or_else(|| Error::Invalid(format!("Invalid ref format: {ref}")))?;
    resolve_ref_node(tab, r#ref, node_id).await?;
    let scroll = ScrollIntoViewIfNeededParams {
        backend_node_id: Some(BackendNodeId::new(node_id)),
        ..Default::default()
    };
    let _ = tab.send(scroll).await;
    let Some(quad) = box_model_quad(tab, node_id).await else {
        return Err(Error::Invalid(format!(
            "element for ref '{ref}' exists but is not rendered (no box model); it cannot be dragged"
        )));
    };
    let from = quad_center(&quad);
    if human_like {
        human::mouse_drag(tab, from, (to_x, to_y), None).await?;
    } else {
        tab.mouse_drag(from, (to_x, to_y), steps).await?;
    }
    human::set_last_mouse_pos(tab, to_x, to_y);
    Ok(json!({"dragged": true, "ref": r#ref}))
}

// ---------------------------------------------------------------------------
// get_element_by_ref + the by-ref tools
// ---------------------------------------------------------------------------

/// Resolve a `ref` into a [`DomElement`] — the bridge between the
/// accessibility tree's name for a node and the DOM layer's. Errors carry
/// the actionable stale-ref message.
pub async fn element_by_ref(tab: &Tab, r#ref: &str) -> Result<DomElement> {
    let node_id = node_id_of(r#ref)
        .ok_or_else(|| Error::Invalid(format!("Invalid ref format (no node_id): {ref}")))?;
    resolve_ref_node(tab, r#ref, node_id).await?;
    Ok(DomElement::new(node_id))
}

fn err_result(key: &str, e: &Error) -> Value {
    json!({key: false, "error": e.to_string()})
}

/// Focus without clicking. `{focused, ref}`.
pub async fn focus_by_ref(tab: &Tab, r#ref: &str) -> Result<Value> {
    match focus_node(tab, r#ref).await {
        Ok(_) => Ok(json!({"focused": true, "ref": r#ref})),
        Err(e) => Ok(err_result("focused", &e)),
    }
}

/// Move the cursor over the element (hover menus, tooltips). `{hovered, ref}`.
pub async fn hover_by_ref(tab: &Tab, r#ref: &str) -> Result<Value> {
    let el = element_by_ref(tab, r#ref).await?;
    el.mouse_move(tab).await?;
    Ok(json!({"hovered": true, "ref": r#ref}))
}

/// Draw a red overlay on the element for `duration` seconds. `{highlighted, ref}`.
pub async fn highlight_by_ref(tab: &Tab, r#ref: &str, duration: f64) -> Result<Value> {
    let el = element_by_ref(tab, r#ref).await?;
    el.highlight_overlay(tab, duration).await?;
    Ok(json!({"highlighted": true, "ref": r#ref}))
}

/// The element's `outerHTML`. `{html, ref}`.
pub async fn html_by_ref(tab: &Tab, r#ref: &str) -> Result<Value> {
    let el = element_by_ref(tab, r#ref).await?;
    let html = el.outer_html(tab).await?;
    Ok(json!({"html": html, "ref": r#ref}))
}

/// Screenshot just the element's box. `{path, size, ref, width, height}`
/// (+ `format`, `capped` when an image cap applies).
pub async fn screenshot_by_ref(
    tab: &Tab,
    r#ref: &str,
    path: Option<&std::path::Path>,
    image_cap: Option<&crate::image_cap::ImageCap>,
) -> Result<Value> {
    let path = if let Some(p) = path {
        p.to_path_buf()
    } else {
        let dir = crate::config::resolve_output_dir();
        std::fs::create_dir_all(&dir)?;
        dir.join(crate::page::timestamp_name("png").replace(".png", "_element.png"))
    };
    let el = element_by_ref(tab, r#ref).await?;
    let bytes = el.save_screenshot(tab, &path).await?;
    let cap = crate::image_cap::resolve_cap(image_cap.cloned());
    if let Some(cap) = cap {
        let r = crate::image_cap::apply_image_cap(&path, &cap, false)?;
        return Ok(json!({
            "path": r.final_path.to_string_lossy(),
            "size": r.final_bytes,
            "ref": r#ref,
            "width": r.final_width,
            "height": r.final_height,
            "format": r.format,
            "capped": r.capped,
        }));
    }
    let (width, height) = crate::geometry::decode_png(&bytes)
        .map(|i| i.dimensions())
        .unwrap_or((0, 0));
    Ok(json!({
        "path": path.to_string_lossy(),
        "size": std::fs::metadata(&path)?.len(),
        "ref": r#ref,
        "width": width,
        "height": height,
    }))
}

/// Select an `<option>` inside a native `<select>`. `{selected, ref}`.
pub async fn select_by_ref(tab: &Tab, r#ref: &str) -> Result<Value> {
    let el = element_by_ref(tab, r#ref).await?;
    el.select_option(tab).await?;
    Ok(json!({"selected": true, "ref": r#ref}))
}

/// Set files on an `<input type="file">`; `paths` is comma-separated.
/// `{uploaded, ref, files}`.
pub async fn upload_by_ref(tab: &Tab, r#ref: &str, paths: &str) -> Result<Value> {
    let el = element_by_ref(tab, r#ref).await?;
    let files: Vec<String> = paths.split(',').map(|p| p.trim().to_string()).collect();
    el.send_files(tab, &files).await?;
    Ok(json!({"uploaded": true, "ref": r#ref, "files": files.len()}))
}

/// Supported key names for [`press_key`], sorted.
#[must_use]
pub fn supported_keys() -> Vec<&'static str> {
    let mut v = vec![
        "arrowdown",
        "arrowleft",
        "arrowright",
        "arrowup",
        "backspace",
        "delete",
        "end",
        "enter",
        "escape",
        "home",
        "pagedown",
        "pageup",
        "space",
        "tab",
    ];
    v.sort_unstable();
    v
}

/// Press a named key (real keyDown/keyUp with `windowsVirtualKeyCode`),
/// optionally focusing `ref` first. `{pressed, key, ref}` or
/// `{pressed: false, reason}`.
pub async fn press_key(tab: &Tab, key: &str, r#ref: Option<&str>, modifiers: i64) -> Result<Value> {
    let Some((dom_key, code, vkey, text)) = key_spec(key) else {
        return Ok(json!({
            "pressed": false,
            "reason": format!("unknown key {key:?}; supported: {}", supported_keys().join(", ")),
        }));
    };
    if let Some(r) = r#ref {
        let Some(node_id) = node_id_of(r) else {
            return Ok(
                json!({"pressed": false, "reason": format!("invalid ref (no node id): {r:?}")}),
            );
        };
        let p = FocusParams {
            backend_node_id: Some(BackendNodeId::new(node_id)),
            ..Default::default()
        };
        if let Err(e) = tab.send(p).await {
            return Ok(json!({"pressed": false, "reason": format!("could not focus {r:?}: {e}")}));
        }
    }
    dispatch_key(tab, dom_key, code, vkey, modifiers, text).await?;
    Ok(json!({"pressed": true, "key": dom_key, "ref": r#ref}))
}
