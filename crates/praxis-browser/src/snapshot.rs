//! `page_discover`: accessibility-tree snapshot + DOM scan. Port of
//! `core/snapshot.py`, kept operation-for-operation identical so refs,
//! roles, names and boxes match the Python implementation on the same page.

use std::collections::{HashMap, HashSet};

use chromiumoxide_cdp::cdp::browser_protocol::accessibility::{
    AxNode, EnableParams as AxEnableParams, GetFullAxTreeParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::dom::{
    BackendNodeId, GetBoxModelParams, GetDocumentParams, Node,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{FrameId, FrameTree, GetFrameTreeParams};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::connection::Tab;
use crate::geometry::py_round;
use crate::refs::{frame_scope, make_ref, node_id_of, scoped_ref};
use crate::Result;

/// Bounding box in CSS px, rounded like Python's `round()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundingBox {
    /// Left edge.
    pub left: i64,
    /// Top edge.
    pub top: i64,
    /// Right edge.
    pub right: i64,
    /// Bottom edge.
    pub bottom: i64,
}

/// One discovered element. Field order == Python dict key order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Element {
    /// `5#214` / `FRAME_ABC:5#214`.
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// AX role, or tag / `role` attribute for DOM-scanned nodes.
    pub role: String,
    /// Accessible name (≤100 chars) or DOM label (≤80 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// AX value (≤50 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Focused state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    /// Disabled state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// Required state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Checked tristate (`true`/`false`/`"mixed"`, as Chrome reports it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<Value>,
    /// Selected state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    /// Expanded state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<Value>,
    /// Heading level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<Value>,
    /// Center X.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<i64>,
    /// Center Y.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<i64>,
    /// Bounding box.
    #[serde(rename = "box", skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BoundingBox>,
    /// Custom `datarole` / `data-role` attribute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datarole: Option<String>,
}

/// Options for [`page_discover`]; defaults match the Python signature.
#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    /// Case-insensitive substring filter on the name.
    pub text: Option<String>,
    /// Only interactive elements (default true).
    pub interactable_only: bool,
    /// Include x/y/box (default true).
    pub include_coordinates: bool,
    /// Include same-origin iframes (default true).
    pub include_iframes: bool,
    /// Also scan the DOM for ARIA-less controls (default true).
    pub dom_scan: bool,
    /// Max DOM-scanned elements (default 200).
    pub dom_limit: usize,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            text: None,
            interactable_only: true,
            include_coordinates: true,
            include_iframes: true,
            dom_scan: true,
            dom_limit: 200,
        }
    }
}

// ---------------------------------------------------------------------------
// Python value semantics helpers
// ---------------------------------------------------------------------------

/// Python truthiness of a JSON value.
fn py_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `str()` of a JSON scalar.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Null => "None".to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(f) = n.as_f64() {
                py_float_repr(f)
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        "nan".to_string()
    } else if f.is_infinite() {
        if f > 0.0 { "inf" } else { "-inf" }.to_string()
    } else if f.fract().abs() < f64::EPSILON && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------------------------------------------------------------------------
// AX tree
// ---------------------------------------------------------------------------

const INTERACTABLE_ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "checkbox",
    "radio",
    "combobox",
    "listbox",
    "option",
    "menuitem",
    "tab",
    "switch",
    "slider",
    "spinbutton",
    "searchbox",
    "menu",
    "menubar",
];

fn ax_value_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// `_format_ax_node` for one node of the *flat* `getFullAXTree` list.
/// (The Python version's child recursion never fires: CDP AX nodes carry
/// `childIds`, not `children`.)
fn format_ax_node(node: &AxNode, counter: &mut usize, interactable_only: bool) -> Option<Element> {
    let mut props: HashMap<String, Option<Value>> = HashMap::new();
    if let Some(list) = &node.properties {
        for p in list {
            props.insert(p.name.as_ref().to_string(), p.value.value.clone());
        }
    }
    let role_raw = node.role.as_ref().and_then(|r| r.value.as_ref());
    let role = role_raw.and_then(ax_value_str);
    let name_raw = node.name.as_ref().and_then(|n| n.value.as_ref());

    if matches!(
        role.as_deref(),
        Some("none" | "generic" | "InlineTextBox" | "LineBreak")
    ) {
        return None;
    }
    let prop = |k: &str| props.get(k).and_then(Option::as_ref);
    let is_interactable = role
        .as_deref()
        .is_some_and(|r| INTERACTABLE_ROLES.contains(&r))
        || py_truthy(prop("focusable"));

    if interactable_only
        && !is_interactable
        && !matches!(role.as_deref(), Some("heading" | "image" | "img" | "alert"))
    {
        return None;
    }
    // `if role and (name or is_interactable or role == "image")`
    let role_truthy = py_truthy(role_raw);
    if !(role_truthy
        && (py_truthy(name_raw) || is_interactable || role.as_deref() == Some("image")))
    {
        return None;
    }
    *counter += 1;
    let node_id = node.backend_dom_node_id.as_ref().map(|id| *id.inner());
    let role_s = role.unwrap_or_else(|| role_raw.map(py_str).unwrap_or_default());
    let mut el = Element {
        r#ref: make_ref(*counter, node_id),
        role: role_s,
        ..Element::default()
    };
    if py_truthy(name_raw) {
        el.name = name_raw.map(|n| take_chars(&py_str(n), 100));
    }
    if let Some(v) = &node.value {
        let val = v.value.as_ref().map(py_str).unwrap_or_default();
        if !val.is_empty() {
            el.value = Some(take_chars(&val, 50));
        }
    }
    if py_truthy(prop("focused")) {
        el.focused = Some(true);
    }
    if py_truthy(prop("disabled")) {
        el.disabled = Some(true);
    }
    if py_truthy(prop("required")) {
        el.required = Some(true);
    }
    if let Some(c) = prop("checked") {
        el.checked = Some(c.clone());
    }
    if py_truthy(prop("selected")) {
        el.selected = Some(true);
    }
    if let Some(e) = prop("expanded") {
        el.expanded = Some(e.clone());
    }
    if el.role == "heading" && py_truthy(prop("level")) {
        el.level = prop("level").cloned();
    }
    Some(el)
}

/// One frame's formatted nodes with refs, optionally scope-prefixed.
async fn frame_nodes(
    tab: &Tab,
    frame_id: Option<&str>,
    interactable_only: bool,
    prefix: Option<&crate::refs::ScopePrefix>,
) -> Result<Vec<Element>> {
    let mut params = GetFullAxTreeParams::default();
    if let Some(f) = frame_id {
        params.frame_id = Some(FrameId::new(f));
    }
    let r = tab.send(params).await?;
    let mut counter = 0usize;
    let mut out = Vec::new();
    for node in &r.nodes {
        if node.role.is_none() {
            continue;
        }
        if let Some(el) = format_ax_node(node, &mut counter, interactable_only) {
            out.push(el);
        }
    }
    if let Some(p) = prefix {
        for el in &mut out {
            el.r#ref = scoped_ref(p, &el.r#ref);
        }
    }
    let mut seen = HashSet::new();
    out.retain(|e| seen.insert(e.r#ref.clone()));
    Ok(out)
}

/// `(id, url, is_main)` for every frame, pre-order.
async fn all_frames(tab: &Tab) -> Vec<(String, String, bool)> {
    fn collect(t: &FrameTree, is_main: bool, out: &mut Vec<(String, String, bool)>) {
        out.push((t.frame.id.inner().clone(), t.frame.url.clone(), is_main));
        if let Some(children) = &t.child_frames {
            for c in children {
                collect(c, false, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Ok(r) = tab.send(GetFrameTreeParams::default()).await {
        collect(&r.frame_tree, true, &mut out);
    }
    out
}

/// Accessibility snapshot: main frame, then every non-`about:blank` iframe
/// with `FRAME_xxxxxxxx:` refs. Pass `frame_id` to snapshot one frame only.
pub async fn get_snapshot(
    tab: &Tab,
    interactable_only: bool,
    frame_id: Option<&str>,
    include_iframes: bool,
) -> Result<Vec<Element>> {
    tab.send(AxEnableParams::default()).await?;
    if let Some(f) = frame_id {
        return frame_nodes(tab, Some(f), interactable_only, None).await;
    }
    let mut all = frame_nodes(tab, None, interactable_only, None).await?;
    if !include_iframes {
        return Ok(all);
    }
    for (id, url, is_main) in all_frames(tab).await {
        if is_main || url == "about:blank" {
            continue;
        }
        let scope = frame_scope(&id);
        if let Ok(nodes) = frame_nodes(tab, Some(&id), interactable_only, Some(&scope)).await {
            all.extend(nodes);
        }
    }
    Ok(all)
}

// ---------------------------------------------------------------------------
// DOM scan
// ---------------------------------------------------------------------------

const ACTIONABLE_TAGS: &[&str] = &["input", "textarea", "select", "button", "a"];
const ACTIONABLE_ATTRS: &[&str] = &[
    "onclick",
    "datarole",
    "data-role",
    "role",
    "tabindex",
    "contenteditable",
];
const ACTIONABLE_CLASS: &[&str] = &[
    "cell", "row", "grid", "kd-", "k-icon", "check", "switch", "toggle", "radio",
];

fn node_attr<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    let attrs = node.attributes.as_ref()?;
    attrs
        .chunks(2)
        .find(|c| c.len() == 2 && c[0] == name)
        .map(|c| c[1].as_str())
}

fn is_actionable(node: &Node) -> bool {
    if ACTIONABLE_TAGS.contains(&node.node_name.to_lowercase().as_str()) {
        return true;
    }
    if ACTIONABLE_ATTRS
        .iter()
        .any(|a| node_attr(node, a).is_some())
    {
        return true;
    }
    let class = node_attr(node, "class").unwrap_or("").to_lowercase();
    ACTIONABLE_CLASS.iter().any(|c| class.contains(c))
}

/// Depth-first walk over `children` (+ first shadow root), like
/// `_element.filter_recurse_all`. Iframe content documents are not entered.
fn filter_recurse_all<'a>(doc: &'a Node, pred: &dyn Fn(&Node) -> bool, out: &mut Vec<&'a Node>) {
    let Some(children) = &doc.children else {
        return;
    };
    for child in children {
        if pred(child) {
            out.push(child);
        }
        if let Some(first) = child.shadow_roots.as_ref().and_then(|s| s.first()) {
            filter_recurse_all(first, pred, out);
        }
        filter_recurse_all(child, pred, out);
    }
}

fn node_label(node: &Node) -> String {
    for attr in ["aria-label", "placeholder", "value", "title", "datarole"] {
        if let Some(v) = node_attr(node, attr) {
            if !v.trim().is_empty() {
                return take_chars(v.trim(), 80);
            }
        }
    }
    let mut texts = Vec::new();
    filter_recurse_all(node, &|n| n.node_type == 3, &mut texts);
    let joined = texts
        .iter()
        .filter(|t| !t.node_value.is_empty())
        .map(|t| t.node_value.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let collapsed = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        take_chars(node_attr(node, "name").unwrap_or("").trim(), 80)
    } else {
        take_chars(&collapsed, 80)
    }
}

/// Quad → (left, top, right, bottom) in CSS px.
fn quad_bounds(q: &[f64]) -> Option<(f64, f64, f64, f64)> {
    if q.len() < 8 {
        return None;
    }
    let xs = [q[0], q[2], q[4], q[6]];
    let ys = [q[1], q[3], q[5], q[7]];
    let min = |a: &[f64]| a.iter().copied().fold(f64::INFINITY, f64::min);
    let max = |a: &[f64]| a.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some((min(&xs), min(&ys), max(&xs), max(&ys)))
}

/// Content quad of a backend node, or `None` if not rendered.
pub async fn box_model_quad(tab: &Tab, backend_node_id: i64) -> Option<Vec<f64>> {
    let p = GetBoxModelParams {
        backend_node_id: Some(BackendNodeId::new(backend_node_id)),
        ..Default::default()
    };
    let r = tab.send(p).await.ok()?;
    let q = r.model.content.inner().clone();
    (q.len() >= 8).then_some(q)
}

async fn dom_scan(tab: &Tab, text: Option<&str>, limit: usize) -> Result<Vec<Element>> {
    let p = GetDocumentParams {
        depth: Some(-1),
        pierce: Some(true),
    };
    let doc = tab.send(p).await?.root;
    let text_l = text.map(str::to_lowercase);
    let mut nodes = Vec::new();
    filter_recurse_all(&doc, &is_actionable, &mut nodes);

    let mut results: Vec<Element> = Vec::new();
    let mut seen: HashSet<(i64, i64, i64, i64, String)> = HashSet::new();
    for node in nodes {
        if results.len() >= limit {
            break;
        }
        let label = node_label(node);
        if let Some(t) = &text_l {
            if !label.to_lowercase().contains(t.as_str()) {
                continue;
            }
        }
        let backend = *node.backend_node_id.inner();
        let Some(q) = box_model_quad(tab, backend).await else {
            continue;
        };
        let Some((left, top, right, bottom)) = quad_bounds(&q) else {
            continue;
        };
        if right - left < 3.0 || bottom - top < 3.0 {
            continue;
        }
        let key = (
            py_round(left),
            py_round(top),
            py_round(right - left),
            py_round(bottom - top),
            label.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        let role = node_attr(node, "role")
            .filter(|s| !s.is_empty())
            .or_else(|| node_attr(node, "data-role").filter(|s| !s.is_empty()))
            .or_else(|| node_attr(node, "datarole").filter(|s| !s.is_empty()))
            .map_or_else(|| node.node_name.to_lowercase(), str::to_string);
        let datarole = node_attr(node, "datarole")
            .filter(|s| !s.is_empty())
            .or_else(|| node_attr(node, "data-role").filter(|s| !s.is_empty()))
            .map(str::to_string);
        results.push(Element {
            r#ref: make_ref(results.len() + 1, Some(backend)),
            role,
            name: Some(label),
            x: Some(py_round(f64::midpoint(left, right))),
            y: Some(py_round(f64::midpoint(top, bottom))),
            bbox: Some(BoundingBox {
                left: py_round(left),
                top: py_round(top),
                right: py_round(right),
                bottom: py_round(bottom),
            }),
            datarole,
            ..Element::default()
        });
    }
    Ok(results)
}

/// Broad discovery of interactable elements (AX tree + DOM scan), each with
/// a `ref` usable by `click_by_ref` / `type_by_ref`.
pub async fn page_discover(tab: &Tab, opts: &DiscoverOptions) -> Result<Vec<Element>> {
    let mut elements =
        get_snapshot(tab, opts.interactable_only, None, opts.include_iframes).await?;

    if let Some(t) = &opts.text {
        let t = t.to_lowercase();
        elements.retain(|e| e.name.as_deref().unwrap_or("").to_lowercase().contains(&t));
    }

    if opts.include_coordinates {
        for el in &mut elements {
            let Some(node_id) = node_id_of(&el.r#ref).filter(|n| *n != 0) else {
                continue;
            };
            let Some(q) = box_model_quad(tab, node_id).await else {
                continue;
            };
            let Some((left, top, right, bottom)) = quad_bounds(&q) else {
                continue;
            };
            el.x = Some(py_round((q[0] + q[2] + q[4] + q[6]) / 4.0));
            el.y = Some(py_round((q[1] + q[3] + q[5] + q[7]) / 4.0));
            el.bbox = Some(BoundingBox {
                left: py_round(left),
                top: py_round(top),
                right: py_round(right),
                bottom: py_round(bottom),
            });
        }
    }

    if opts.dom_scan {
        let mut by_backend: HashMap<i64, usize> = HashMap::new();
        for (i, e) in elements.iter().enumerate() {
            if let Some(n) = node_id_of(&e.r#ref) {
                by_backend.entry(n).or_insert(i);
            }
        }
        for de in dom_scan(tab, opts.text.as_deref(), opts.dom_limit).await? {
            let existing = node_id_of(&de.r#ref).and_then(|n| by_backend.get(&n).copied());
            match existing {
                None => elements.push(de),
                Some(i) => {
                    let ex = &mut elements[i];
                    if de.datarole.is_some() && ex.datarole.is_none() {
                        ex.datarole.clone_from(&de.datarole);
                    }
                    if ex.name.as_deref().unwrap_or("").is_empty()
                        && de.name.as_deref().is_some_and(|n| !n.is_empty())
                    {
                        ex.name.clone_from(&de.name);
                    }
                }
            }
        }
    }
    Ok(elements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_str_semantics() {
        assert_eq!(py_str(&Value::Bool(true)), "True");
        assert_eq!(py_str(&serde_json::json!(3)), "3");
        assert_eq!(py_str(&serde_json::json!(2.5)), "2.5");
        assert_eq!(py_str(&serde_json::json!("x")), "x");
    }

    #[test]
    fn truthiness() {
        assert!(!py_truthy(Some(&serde_json::json!(""))));
        assert!(!py_truthy(Some(&serde_json::json!(0))));
        assert!(py_truthy(Some(&serde_json::json!("a"))));
        assert!(!py_truthy(None));
    }

    #[test]
    fn element_json_shape() {
        let e = Element {
            r#ref: "1#5".into(),
            role: "button".into(),
            name: Some("Go".into()),
            x: Some(1),
            y: Some(2),
            bbox: Some(BoundingBox {
                left: 0,
                top: 0,
                right: 2,
                bottom: 4,
            }),
            ..Element::default()
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(
            s,
            r#"{"ref":"1#5","role":"button","name":"Go","x":1,"y":2,"box":{"left":0,"top":0,"right":2,"bottom":4}}"#
        );
    }
}
