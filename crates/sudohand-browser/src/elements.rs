//! Locator tools — port of `core/elements.py`: `find_by_text`,
//! `find_by_html_id`, `find_by_xpath`, `click_by_html_id`, `click_by_xpath`,
//! `click_row_by_text`, `type_by_text`, `select_text`, `page_scroll`,
//! `page_wait_element`.
//!
//! The JS snippets are copied verbatim from the Python module so both
//! implementations locate the same node.

use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::input::{DispatchMouseEventType, MouseButton};
use serde_json::{json, Map, Value};

use crate::actions::{
    ax_by_text, capture_page_state, dispatch_key, element_by_ref, failed, key_spec,
    wait_ax_by_text, with_nav_feedback,
};
use crate::connection::Tab;
use crate::element::{find_element_by_text, find_with_timeout, query_selector_all, DomElement};
use crate::human::{self, dispatch_mouse};
use crate::refs::make_ref;
use crate::Result;

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

// ---------------------------------------------------------------------------
// find_by_text / type_by_text
// ---------------------------------------------------------------------------

/// Locate by accessible name (two-tier). `{found, ref, role, name, x, y, …}`
/// or `{found: false, text}`.
pub async fn find_by_text(tab: &Tab, text: &str, interactable_only: bool) -> Result<Value> {
    match ax_by_text(tab, text, interactable_only).await? {
        None => Ok(json!({"found": false, "text": text})),
        Some(el) => {
            let mut m = Map::new();
            m.insert("found".into(), json!(true));
            if let Value::Object(fields) = serde_json::to_value(&el)? {
                m.extend(fields);
            }
            Ok(Value::Object(m))
        }
    }
}

/// Options for [`type_by_text`].
#[derive(Debug, Clone, Copy)]
pub struct TypeByTextOptions {
    /// `el.value = ""` first.
    pub clear: bool,
    /// Seconds to wait for the name to appear.
    pub timeout: f64,
    /// Human timing between keystrokes (`None` = config `type_humanize`).
    pub human_like: Option<bool>,
    /// Press Enter afterwards.
    pub enter: bool,
    /// Real per-character key events (wins over `human_like`).
    pub keystrokes: bool,
}

impl Default for TypeByTextOptions {
    fn default() -> Self {
        Self {
            clear: false,
            timeout: 10.0,
            human_like: None,
            enter: false,
            keystrokes: false,
        }
    }
}

/// Locate an input by accessible name and type into it. Default actuator is
/// per-character `char` events (not `insertText`). `{typed, name, ref}`
/// (+ `entered`).
pub async fn type_by_text(
    tab: &Tab,
    name: &str,
    text: &str,
    opts: TypeByTextOptions,
) -> Result<Value> {
    let Some(located) = wait_ax_by_text(tab, name, opts.timeout).await? else {
        return Ok(
            json!({"typed": false, "error": format!("Element with name '{name}' not found")}),
        );
    };
    let Ok(element) = element_by_ref(tab, &located.r#ref).await else {
        return Ok(
            json!({"typed": false, "error": format!("Element with name '{name}' not found")}),
        );
    };
    if opts.clear {
        element.clear_input(tab).await?;
    }
    if opts.keystrokes {
        element.focus(tab).await?;
        for ch in text.chars() {
            let s = ch.to_string();
            dispatch_key(tab, &s, "", 0, 0, Some(&s)).await?;
        }
    } else {
        let use_human = opts
            .human_like
            .unwrap_or_else(|| human::get_config().type_humanize);
        if use_human {
            element.focus(tab).await?;
            human::type_text(tab, text, Some(true)).await?;
        } else {
            element.send_keys(tab, text).await?;
        }
    }
    let mut out = Map::new();
    out.insert("typed".into(), json!(true));
    out.insert("name".into(), json!(name));
    out.insert("ref".into(), json!(located.r#ref));
    if opts.enter {
        let pressed = crate::actions::press_key(tab, "Enter", Some(&located.r#ref), 0).await?;
        out.insert(
            "entered".into(),
            json!(pressed.get("pressed") == Some(&json!(true))),
        );
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// page_scroll
// ---------------------------------------------------------------------------

const SCROLL_TO_EDGE_JS: &str = r"(() => {
  const DIR = '__DIR__';
  const MIN = 4;  // scrollHeight - clientHeight must exceed this to count

  const roomOf = (el) => el.scrollHeight - el.clientHeight;

  const describe = (el, win, root) => {
    if (el === root) return win === window ? 'window' : 'frame-document';
    const id = el.id ? '#' + el.id : '';
    const cls = (typeof el.className === 'string' && el.className.trim())
      ? '.' + el.className.trim().split(/\s+/).slice(0, 2).join('.') : '';
    return (el.tagName || 'node').toLowerCase() + id + cls;
  };

  const candidates = [];
  let crossOrigin = false;

  const collect = (win, prefix) => {
    let doc;
    try { doc = win.document; } catch (e) { crossOrigin = true; return; }
    if (!doc) return;
    const root = doc.scrollingElement || doc.documentElement || doc.body;
    if (root && roomOf(root) > MIN) {
      candidates.push({ el: root, win, room: roomOf(root),
                        label: prefix + describe(root, win, root), top: prefix === '' });
    }
    for (const el of doc.querySelectorAll('*')) {
      if (el === root || roomOf(el) <= MIN) continue;
      const oy = win.getComputedStyle(el).overflowY;
      if (oy === 'auto' || oy === 'scroll' || oy === 'overlay') {
        candidates.push({ el, win, room: roomOf(el),
                          label: prefix + describe(el, win, root), top: false });
      }
    }
    for (const frame of doc.querySelectorAll('iframe, frame')) {
      let cw;
      try { cw = frame.contentWindow; if (cw) void cw.document; }
      catch (e) { crossOrigin = true; continue; }
      if (!cw) continue;
      const tag = frame.id ? '#' + frame.id
                  : (frame.name ? '[name=' + frame.name + ']' : '');
      collect(cw, prefix + 'iframe' + tag + ' > ');
    }
  };

  collect(window, '');
  if (candidates.length === 0) return { found: false, crossOrigin };

  let t = candidates.find((c) => c.top);
  if (!t) t = candidates.reduce((a, b) => (b.room > a.room ? b : a));

  const el = t.el;
  const before = el.scrollTop;
  el.scrollTop = DIR === 'bottom' ? el.scrollHeight : 0;
  return {
    found: true, target: t.label, before, after: el.scrollTop,
    delta: el.scrollTop - before, crossOrigin, candidates: candidates.length,
  };
})()";

/// Scroll target for [`page_scroll`].
#[derive(Debug, Clone, Default)]
pub struct ScrollOptions {
    /// `"up"` / `"down"` for the incremental gesture (default down).
    pub direction: String,
    /// Percent of the viewport height for the gesture (default 25).
    pub amount: i64,
    /// Scroll the real scroll container to its bottom.
    pub to_bottom: bool,
    /// … or its top.
    pub to_top: bool,
    /// Visible text of an element to scroll into view.
    pub to_element: Option<String>,
}

async fn resolve_scroll_target(tab: &Tab, target: &str) -> Result<Option<DomElement>> {
    if let Some(hit) = ax_by_text(tab, target, false).await? {
        if let Ok(el) = element_by_ref(tab, &hit.r#ref).await {
            return Ok(Some(el));
        }
    }
    Ok(find_with_timeout(tab, target, true, 3.0)
        .await?
        .map(|h| h.element))
}

/// Scroll: to an element by text, to the real container's edge, or an
/// incremental gesture. `{scrolled: true, …}` or `{scrolled: false, reason}`.
pub async fn page_scroll(tab: &Tab, opts: &ScrollOptions) -> Result<Value> {
    if let Some(target) = &opts.to_element {
        let Some(el) = resolve_scroll_target(tab, target).await? else {
            return Ok(json!({
                "scrolled": false,
                "reason": format!("no element found to scroll to for {target:?}"),
            }));
        };
        el.scroll_into_view(tab).await;
        return Ok(json!({"scrolled": true, "target": target}));
    }
    if opts.to_bottom || opts.to_top {
        let edge = if opts.to_bottom { "bottom" } else { "top" };
        let info = tab
            .evaluate(&SCROLL_TO_EDGE_JS.replace("__DIR__", edge))
            .await?;
        if info.get("found") != Some(&json!(true)) {
            let reason = if info.get("crossOrigin") == Some(&json!(true)) {
                "the scrollable content is inside a cross-origin iframe that this JS scroll can't reach; try direction='down' (gesture scroll routes to whatever is under the cursor), or scroll it directly with js_evaluate(frame=...)"
            } else {
                "nothing on this page is scrollable (content fits the viewport)"
            };
            return Ok(json!({"scrolled": false, "reason": reason}));
        }
        return Ok(json!({
            "scrolled": true,
            "target": info.get("target"),
            "y": info.get("after"),
            "delta": info.get("delta"),
        }));
    }
    let amount = if opts.amount == 0 { 25 } else { opts.amount };
    let direction = if opts.direction == "up" { "up" } else { "down" };
    let signed = if direction == "up" { -amount } else { amount };
    tab.scroll_by_viewport_percent(signed as f64).await?;
    Ok(json!({"scrolled": true, "direction": direction, "amount": amount}))
}

// ---------------------------------------------------------------------------
// page_wait_element
// ---------------------------------------------------------------------------

const WAIT_PROBE_JS: &str = r"(function (selector, text) {
  var el = null, idx = -1;
  var vis = function (e) {
    var r = e.getBoundingClientRect(), s = getComputedStyle(e);
    return r.width > 2 && r.height > 2 &&
           s.visibility !== 'hidden' && s.display !== 'none';
  };
  if (selector) {
    var els = document.querySelectorAll(selector);
    for (var i = 0; i < els.length; i++) {
      if (vis(els[i])) { el = els[i]; idx = i; break; }
    }
  } else if (text) {
    var w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null);
    var n;
    while ((n = w.nextNode())) {
      if ((n.nodeValue || '').indexOf(text) !== -1) {
        var p = n.parentElement;
        if (p && vis(p)) { el = p; break; }
      }
    }
  }
  if (!el) return null;
  var r = el.getBoundingClientRect();
  return {
    idx: idx,
    left: Math.round(r.left), top: Math.round(r.top),
    right: Math.round(r.right), bottom: Math.round(r.bottom),
  };
})(__SELECTOR__, __TEXT__)";

async fn wait_probe(tab: &Tab, text: Option<&str>, selector: Option<&str>) -> Option<Value> {
    let js = WAIT_PROBE_JS
        .replace("__SELECTOR__", &selector.map_or("null".to_string(), js_str))
        .replace("__TEXT__", &text.map_or("null".to_string(), js_str));
    match tab.evaluate(&js).await {
        Ok(Value::Null) | Err(_) => None,
        Ok(v) => Some(v),
    }
}

async fn resolve_wait_ref(
    tab: &Tab,
    text: Option<&str>,
    selector: Option<&str>,
    idx: i64,
) -> Result<Option<(String, String, Option<String>)>> {
    let (element, text_all) = if let Some(sel) = selector {
        let els = query_selector_all(tab, sel).await?;
        if els.is_empty() {
            return Ok(None);
        }
        let i = usize::try_from(idx).unwrap_or(0);
        let el = els.get(i).or_else(|| els.first()).cloned();
        let Some(el) = el else { return Ok(None) };
        let node = crate::element::describe(tab, el.backend_node_id, -1).await?;
        (el, crate::element::text_all(&node))
    } else if let Some(t) = text {
        match find_element_by_text(tab, t, false).await? {
            Some(h) => (h.element, h.text_all),
            None => return Ok(None),
        }
    } else {
        return Ok(None);
    };
    let name: String = text_all.trim().chars().take(80).collect();
    Ok(Some((
        make_ref(1, Some(element.backend_node_id)),
        element.node_name.to_lowercase(),
        (!name.is_empty()).then_some(name),
    )))
}

/// Wait for an element to be *visible* (two consecutive polls) by CSS
/// selector or visible text. `{found, ref, role, name, x, y, box, elapsed}`
/// or `{found: false, elapsed, message}`.
pub async fn page_wait_element(
    tab: &Tab,
    text: Option<&str>,
    selector: Option<&str>,
    timeout: f64,
) -> Result<Value> {
    let start = tokio::time::Instant::now();
    let mut was_visible = false;
    loop {
        match wait_probe(tab, text, selector).await {
            Some(probe) => {
                if was_visible {
                    let idx = probe.get("idx").and_then(Value::as_i64).unwrap_or(-1);
                    if let Some((r#ref, role, name)) =
                        resolve_wait_ref(tab, text, selector, idx).await?
                    {
                        let num = |k: &str| probe.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                        let (left, top, right, bottom) =
                            (num("left"), num("top"), num("right"), num("bottom"));
                        return Ok(json!({
                            "found": true,
                            "ref": r#ref,
                            "role": role,
                            "name": name,
                            "x": crate::geometry::py_round(f64::midpoint(left, right)),
                            "y": crate::geometry::py_round(f64::midpoint(top, bottom)),
                            "box": {"left": left, "top": top, "right": right, "bottom": bottom},
                            "elapsed": (start.elapsed().as_secs_f64() * 100.0).round() / 100.0,
                        }));
                    }
                }
                was_visible = true;
            }
            None => was_visible = false,
        }
        if start.elapsed().as_secs_f64() > timeout {
            return Ok(json!({
                "found": false,
                "elapsed": (start.elapsed().as_secs_f64() * 100.0).round() / 100.0,
                "message": format!("Timeout after {timeout}s (not visible)"),
            }));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

// ---------------------------------------------------------------------------
// click_row_by_text
// ---------------------------------------------------------------------------

const FIND_ROW_JS: &str = r"(function (needle, nth, mode) {
  const cand = Array.prototype.slice
    .call(document.querySelectorAll('[class*=row],[role=row],tr'))
    .filter(function (el) { return (el.innerText || '').indexOf(needle) !== -1; })
    .filter(function (el) {
      const r = el.getBoundingClientRect();
      return r.width > 20 && r.height > 2;
    });
  const set = new Set(cand);
  const outer = cand.filter(function (el) {
    let p = el.parentElement;
    while (p) { if (set.has(p)) return false; p = p.parentElement; }
    return true;
  });
  const row = outer[nth];
  if (!row) return null;
  const vis = function (el) {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    return r.width > 2 && r.height > 2;
  };
  const scrollTo = function (el) {
    try {
      if (el.scrollIntoViewIfNeeded) el.scrollIntoViewIfNeeded(true);
      else el.scrollIntoView({ block: 'center', inline: 'nearest' });
    } catch (e) {}
  };
  const cb = row.querySelector('input[type=checkbox],input[type=radio]');

  if (mode === 'verify') {
    return { checked: cb ? !!cb.checked : null,
             ariaSelected: row.getAttribute('aria-selected') };
  }

  const info = {
    matches: outer.length,
    text: (row.innerText || '').replace(/\s+/g, ' ').trim().slice(0, 80),
  };

  if (mode !== 'checkbox') {
    scrollTo(row);
    const r = row.getBoundingClientRect();
    info.x = Math.round(r.left + r.width / 2);
    info.y = Math.round(r.top + r.height / 2);
    return info;
  }

  let target = null;
  const roleCb = row.querySelector(
    '[role=checkbox],[data-role=checkbox],[datarole=checkbox]');
  if (cb) {
    info.checkedBefore = !!cb.checked;
    let wrapper = null, n = cb;
    for (let i = 0; i < 5 && n; i++) {
      n = n.parentElement;
      if (!n || n === row) break;
      if (n.matches('[role=checkbox],[data-role=checkbox],[datarole=checkbox]') ||
          /check|switch|toggle/i.test(n.className || '')) { wrapper = n; break; }
    }
    let label = null;
    try {
      label = cb.id
        ? row.querySelector('label[for=\x22' +
            (window.CSS && CSS.escape ? CSS.escape(cb.id) : cb.id) + '\x22]')
        : cb.closest('label');
    } catch (e) {}
    const locked = cb.hasAttribute('onclick');
    target = (locked && vis(wrapper)) ? wrapper
      : (vis(cb) ? cb : (vis(wrapper) ? wrapper : (vis(label) ? label
      : (wrapper || cb))));
  } else if (roleCb) {
    target = roleCb;
    info.checkedBefore = roleCb.getAttribute('aria-checked') === 'true';
  }
  if (!target) { info.nocheckbox = true; return info; }
  scrollTo(target);
  const tr = target.getBoundingClientRect();
  info.x = Math.round(tr.left + tr.width / 2);
  info.y = Math.round(tr.top + tr.height / 2);
  return info;
})(__NEEDLE__, __NTH__, __MODE__)";

/// Act on a grid row containing `text`: click / double-click it, or toggle
/// its checkbox via the widget that really handles the toggle.
pub async fn click_row_by_text(
    tab: &Tab,
    text: &str,
    double: bool,
    nth: i64,
    checkbox: bool,
) -> Result<Value> {
    let row_js = |mode: &str| {
        FIND_ROW_JS
            .replace("__NEEDLE__", &js_str(text))
            .replace("__NTH__", &nth.to_string())
            .replace("__MODE__", &js_str(mode))
    };
    let hit = tab
        .evaluate(&row_js(if checkbox { "checkbox" } else { "row" }))
        .await?;
    if hit.is_null() {
        return Ok(json!({"clicked": false, "reason": format!("no grid row containing {text:?}")}));
    }
    if checkbox && hit.get("nocheckbox") == Some(&json!(true)) {
        return Ok(json!({
            "clicked": false,
            "reason": format!("row {:?} has no checkbox", hit.get("text").and_then(Value::as_str).unwrap_or("")),
        }));
    }
    let x = hit.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = hit.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    if double && !checkbox {
        for count in [1, 2] {
            dispatch_mouse(
                tab,
                DispatchMouseEventType::MousePressed,
                x,
                y,
                Some(MouseButton::Left),
                Some(count),
                0,
            )
            .await?;
            dispatch_mouse(
                tab,
                DispatchMouseEventType::MouseReleased,
                x,
                y,
                Some(MouseButton::Left),
                Some(count),
                0,
            )
            .await?;
        }
    } else {
        tab.mouse_click(x, y).await?;
    }
    let mut out = Map::new();
    out.insert("clicked".into(), json!(true));
    out.insert(
        "text".into(),
        hit.get("text").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "matches".into(),
        hit.get("matches").cloned().unwrap_or(Value::Null),
    );
    out.insert("x".into(), json!(x));
    out.insert("y".into(), json!(y));
    if checkbox {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let after = tab.evaluate(&row_js("verify")).await.unwrap_or(Value::Null);
        out.insert(
            "was".into(),
            hit.get("checkedBefore").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "checked".into(),
            after.get("checked").cloned().unwrap_or(Value::Null),
        );
    } else {
        out.insert("double".into(), json!(double));
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// select_text
// ---------------------------------------------------------------------------

const SELECT_TEXT_JS: &str = r"
    (function(startText, endText) {
      function findRange(doc) {
        const root = doc.body || doc.documentElement;
        if (!root) return null;
        const walk = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
        let startNode = null, startOff = -1;
        while (walk.nextNode()) {
          const tn = walk.currentNode;
          const i = (tn.nodeValue || '').indexOf(startText);
          if (i >= 0) { startNode = tn; startOff = i; break; }
        }
        if (!startNode) return null;
        const rg = doc.createRange();
        rg.setStart(startNode, startOff);
        if (endText) {
          const walk2 = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
          let seen = false, endNode = null, endOff = -1;
          while (walk2.nextNode()) {
            const tn = walk2.currentNode;
            if (tn === startNode) seen = true;
            if (!seen) continue;
            const from = (tn === startNode) ? startOff : 0;
            const j = (tn.nodeValue || '').indexOf(endText, from);
            if (j >= 0) { endNode = tn; endOff = j + endText.length; break; }
          }
          if (!endNode) return null;
          rg.setEnd(endNode, endOff);
        } else {
          rg.setEnd(startNode, startOff + startText.length);
        }
        return rg;
      }
      function recurse(win, ctx) {
        let doc;
        try { doc = win.document; } catch (e) { return null; }
        let rg = null;
        try { rg = findRange(doc); } catch (e) { rg = null; }
        if (rg) return { win: win, doc: doc, rg: rg, ctx: ctx };
        for (let i = 0; i < win.frames.length; i++) {
          try {
            const hit = recurse(win.frames[i], 'iframe');
            if (hit) return hit;
          } catch (e) {}
        }
        return null;
      }
      const hit = recurse(window, 'top');
      if (!hit) return { selected: false, text: startText };
      const win = hit.win, doc = hit.doc, rg = hit.rg;
      const sel = win.getSelection();
      sel.removeAllRanges();
      sel.addRange(rg);
      const out = sel.toString();
      const collapsed = sel.isCollapsed;
      const ok = sel.rangeCount > 0 && !collapsed && out.length > 0;
      try { doc.dispatchEvent(new win.Event('selectionchange')); } catch (e) {}
      return {
        selected: ok, text: out, query: startText, frame: hit.ctx,
        chars: out.length, collapsed: collapsed
      };
    })(__START__, __END__)
";

/// Build a real text selection over `text` (optionally extended to the end
/// of `to_text`), in the top document or the first same-origin iframe.
pub async fn select_text(tab: &Tab, text: &str, to_text: Option<&str>) -> Result<Value> {
    let expr = SELECT_TEXT_JS
        .replace("__START__", &js_str(text))
        .replace("__END__", &to_text.map_or("null".to_string(), js_str));
    tab.evaluate(&expr).await
}

// ---------------------------------------------------------------------------
// find_by_html_id / find_by_xpath / click_by_html_id / click_by_xpath
// ---------------------------------------------------------------------------

const ELEMENT_INFO_INLINE: &str = r"
        const __elementInfo = (el) => {
          if (!el) return {found: false};
          let rect = {width: 0, height: 0};
          try { rect = el.getBoundingClientRect(); } catch(e) {}
          return {
            found: true,
            tag: (el.tagName || '').toLowerCase(),
            text: ((el.innerText || el.textContent || '') + '').trim().slice(0, 200),
            visible: rect.width > 0 && rect.height > 0 && el.offsetParent !== null,
            attrs: {
              id: el.id || null,
              name: el.getAttribute ? el.getAttribute('name') : null,
              type: el.getAttribute ? el.getAttribute('type') : null,
              'aria-label': el.getAttribute ? el.getAttribute('aria-label') : null,
            }
          };
        };";

/// `{found, tag, text, visible, attrs}` for the element with html `id`,
/// searching same-origin frames.
pub async fn find_by_html_id(tab: &Tab, html_id: &str) -> Result<Value> {
    let expr = format!(
        r"
    (function(id) {{
      {ELEMENT_INFO_INLINE}
      function search(win) {{
        try {{
          const el = win.document.getElementById(id);
          if (el) return el;
        }} catch(e) {{}}
        for (let i = 0; i < win.frames.length; i++) {{
          try {{
            const result = search(win.frames[i]);
            if (result) return result;
          }} catch(e) {{}}
        }}
        return null;
      }}
      return __elementInfo(search(window));
    }})({})
    ",
        js_str(html_id)
    );
    tab.evaluate(&expr).await
}

/// `{found, tag, text, visible, attrs}` for the first XPath match,
/// searching same-origin frames.
pub async fn find_by_xpath(tab: &Tab, xpath: &str) -> Result<Value> {
    let expr = format!(
        r"
    (function(xpath) {{
      {ELEMENT_INFO_INLINE}
      function search(doc) {{
        try {{
          const result = doc.evaluate(xpath, doc, null,
                                      XPathResult.FIRST_ORDERED_NODE_TYPE, null);
          if (result && result.singleNodeValue) return result.singleNodeValue;
        }} catch(e) {{}}
        return null;
      }}
      function recurse(win) {{
        try {{
          const hit = search(win.document);
          if (hit) return hit;
        }} catch(e) {{}}
        for (let i = 0; i < win.frames.length; i++) {{
          try {{
            const hit = recurse(win.frames[i]);
            if (hit) return hit;
          }} catch(e) {{}}
        }}
        return null;
      }}
      return __elementInfo(recurse(window));
    }})({})
    ",
        js_str(xpath)
    );
    tab.evaluate(&expr).await
}

const LOCATE_FOR_CLICK_JS: &str = r"
(function() {
  __FINDER__
  if (!el) return null;
  const ownerWin = el.ownerDocument.defaultView;
  const r0 = el.getBoundingClientRect();
  const wasVisible = r0.width > 0 && r0.height > 0 &&
    r0.top >= 0 && r0.left >= 0 &&
    r0.bottom <= (ownerWin.innerHeight || 1e9) &&
    r0.right <= (ownerWin.innerWidth || 1e9);
  try {
    if (el.scrollIntoViewIfNeeded) el.scrollIntoViewIfNeeded(true);
    else el.scrollIntoView({block: 'center', inline: 'center'});
  } catch (e) {}
  const r = el.getBoundingClientRect();
  let x = r.left + r.width / 2, y = r.top + r.height / 2;
  let win = ownerWin;
  try {
    while (win && win !== window.top && win.frameElement) {
      const fr = win.frameElement.getBoundingClientRect();
      x += fr.left; y += fr.top;
      win = win.parent;
    }
  } catch (e) {}
  return {x: x, y: y, w: r.width, h: r.height, wasVisible: wasVisible};
})()
";

/// A finder that assigns `el` to the first XPath match, recursing
/// same-origin frames (shared with `download_link`).
#[must_use]
pub fn xpath_finder_js(xpath: &str) -> String {
    format!(
        "const XP={};function search(doc){{try{{const r=doc.evaluate(XP,doc,null,XPathResult.FIRST_ORDERED_NODE_TYPE,null);if(r&&r.singleNodeValue)return r.singleNodeValue;}}catch(e){{}}return null;}}function recurse(win){{try{{const h=search(win.document);if(h)return h;}}catch(e){{}}for(let i=0;i<win.frames.length;i++){{try{{const h=recurse(win.frames[i]);if(h)return h;}}catch(e){{}}}}return null;}}const el=recurse(window);",
        js_str(xpath)
    )
}

fn html_id_finder_js(html_id: &str) -> String {
    format!(
        "function search(win){{try{{const el=win.document.getElementById({});if(el)return el;}}catch(e){{}}for(let i=0;i<win.frames.length;i++){{try{{const r=search(win.frames[i]);if(r)return r;}}catch(e){{}}}}return null;}}const el=search(window);",
        js_str(html_id)
    )
}

/// Locate via `finder_js` (must assign `el`) → scroll into view → trusted
/// CDP click at the top-level centre, with navigation feedback.
pub async fn trusted_click(
    tab: &Tab,
    finder_js: &str,
    locator_key: &str,
    locator_val: &str,
) -> Result<Value> {
    let (url_before, _) = capture_page_state(tab).await?;
    let hit = tab
        .evaluate(&LOCATE_FOR_CLICK_JS.replace("__FINDER__", finder_js))
        .await?;
    let mut action = Map::new();
    action.insert("clicked".into(), json!(false));
    action.insert(locator_key.to_string(), json!(locator_val));
    if hit.is_null() {
        action.insert("error".into(), json!("not found"));
        return Ok(Value::Object(failed(action, &url_before)));
    }
    let g = |k: &str| hit.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    if g("w") == 0.0 || g("h") == 0.0 {
        action.insert(
            "error".into(),
            json!("element found but has zero size (not clickable)"),
        );
        return Ok(Value::Object(failed(action, &url_before)));
    }
    tab.mouse_click(g("x"), g("y")).await?;
    action.insert("clicked".into(), json!(true));
    if hit.get("wasVisible") != Some(&json!(true)) {
        action.insert("scrolled_into_view".into(), json!(true));
    }
    action.insert("url_before".into(), json!(url_before));
    Ok(Value::Object(with_nav_feedback(tab, action).await))
}

/// Trusted click on the element with html `id` (same-origin frames included).
pub async fn click_by_html_id(tab: &Tab, html_id: &str) -> Result<Value> {
    trusted_click(tab, &html_id_finder_js(html_id), "html_id", html_id).await
}

/// Trusted click on the first XPath match (same-origin frames included).
pub async fn click_by_xpath(tab: &Tab, xpath: &str) -> Result<Value> {
    trusted_click(tab, &xpath_finder_js(xpath), "xpath", xpath).await
}

/// Re-export so `press_key`'s key table is reachable from here.
#[must_use]
pub fn is_known_key(name: &str) -> bool {
    key_spec(name).is_some()
}
