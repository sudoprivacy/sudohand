//! DOM element operations keyed by backend node id — port of `core/_element.py`.
//!
//! A [`DomElement`] is the DOM-layer name for a node (the accessibility
//! layer's name is the `ref`; [`crate::actions::element_by_ref`] bridges the
//! two). Every method re-resolves the node so coordinates are fresh after a
//! scroll, exactly as the Python `Element` does.

use std::path::Path;
use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, DiscardSearchResultsParams, GetContentQuadsParams,
    GetOuterHtmlParams, GetSearchResultsParams, Node, NodeId, PerformSearchParams,
    QuerySelectorAllParams, ResolveNodeParams, Rgba, ScrollIntoViewIfNeededParams,
    SetFileInputFilesParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType,
};
use chromiumoxide_cdp::cdp::browser_protocol::overlay::{
    EnableParams as OverlayEnableParams, HideHighlightParams, HighlightConfig, HighlightNodeParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams, Viewport,
};
use chromiumoxide_cdp::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use serde_json::Value;

use crate::connection::Tab;
use crate::{Error, Result};

/// Element geometry from the first content quad, in CSS viewport px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// Left edge.
    pub left: f64,
    /// Top edge.
    pub top: f64,
    /// Right edge.
    pub right: f64,
    /// Bottom edge.
    pub bottom: f64,
}

impl Position {
    /// From an 8-value quad `[x1,y1, x2,y2, x3,y3, x4,y4]` (Python reads
    /// left/top from point 1 and right/bottom from point 3).
    #[must_use]
    pub fn from_quad(q: &[f64]) -> Self {
        Self {
            left: q[0],
            top: q[1],
            right: q[4],
            bottom: q[5],
        }
    }
    /// Width.
    #[must_use]
    pub fn width(&self) -> f64 {
        self.right - self.left
    }
    /// Height.
    #[must_use]
    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }
    /// Centre point.
    #[must_use]
    pub fn center(&self) -> (f64, f64) {
        (
            self.left + self.width() / 2.0,
            self.top + self.height() / 2.0,
        )
    }
}

/// A DOM node addressed by its backend node id.
#[derive(Debug, Clone)]
pub struct DomElement {
    /// CDP backend node id — stable across `DOM.getDocument` calls.
    pub backend_node_id: i64,
    /// `nodeName` as of the last describe (may be empty if never described).
    pub node_name: String,
}

impl DomElement {
    /// Wrap a backend node id.
    #[must_use]
    pub fn new(backend_node_id: i64) -> Self {
        Self {
            backend_node_id,
            node_name: String::new(),
        }
    }

    fn id(&self) -> BackendNodeId {
        BackendNodeId::new(self.backend_node_id)
    }

    /// Resolve to a JS object id.
    pub async fn object_id(&self, tab: &Tab) -> Result<String> {
        let rp = ResolveNodeParams {
            backend_node_id: Some(self.id()),
            ..Default::default()
        };
        let obj = tab.send(rp).await?.object;
        obj.object_id
            .map(|o| o.inner().clone())
            .ok_or_else(|| Error::Invalid("node did not resolve to a JS object".to_string()))
    }

    /// Run `js_function` (a function expression taking the element) on this
    /// node and return the value (`return_by_value`). A page-side throw is an
    /// error, like `Tab.evaluate`.
    pub async fn apply(&self, tab: &Tab, js_function: &str) -> Result<Value> {
        let oid = self.object_id(tab).await?;
        let mut call = CallFunctionOnParams::builder()
            .function_declaration(js_function)
            .object_id(oid.clone())
            .return_by_value(true)
            .user_gesture(true)
            .build()
            .map_err(Error::Invalid)?;
        call.arguments = Some(vec![CallArgument::builder().object_id(oid).build()]);
        let r = tab.send(call).await?;
        crate::js::unwrap(&r.result, r.exception_details.as_ref(), Some(js_function))
            .map_err(Error::JsEvaluation)
    }

    /// `DOM.scrollIntoViewIfNeeded` (best effort).
    pub async fn scroll_into_view(&self, tab: &Tab) {
        let _ = tab
            .send(ScrollIntoViewIfNeededParams {
                backend_node_id: Some(self.id()),
                ..Default::default()
            })
            .await;
    }

    /// Fresh viewport geometry from `DOM.getContentQuads`.
    pub async fn position(&self, tab: &Tab) -> Result<Position> {
        let oid = self.object_id(tab).await?;
        let r = tab
            .send(GetContentQuadsParams {
                object_id: Some(oid.into()),
                ..Default::default()
            })
            .await?;
        let q = r
            .quads
            .first()
            .map(|q| q.inner().clone())
            .filter(|q| q.len() >= 8)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "Could not find position for node {}",
                    self.backend_node_id
                ))
            })?;
        Ok(Position::from_quad(&q))
    }

    /// Focus via JS.
    pub async fn focus(&self, tab: &Tab) -> Result<()> {
        self.apply(tab, "(element) => element.focus()").await?;
        Ok(())
    }

    /// `el.value = ""`.
    pub async fn clear_input(&self, tab: &Tab) -> Result<()> {
        self.apply(tab, "function (element) { element.value = \"\" }")
            .await?;
        Ok(())
    }

    /// Focus, then per-character `char` key events.
    pub async fn send_keys(&self, tab: &Tab, text: &str) -> Result<()> {
        self.apply(tab, "(elem) => elem.focus()").await?;
        for ch in text.chars() {
            let p = DispatchKeyEventParams::builder()
                .r#type(DispatchKeyEventType::Char)
                .text(ch.to_string())
                .build()
                .map_err(Error::Invalid)?;
            tab.send(p).await?;
        }
        Ok(())
    }

    /// JS `el.click()` (not trusted).
    pub async fn click_js(&self, tab: &Tab) -> Result<()> {
        let oid = self.object_id(tab).await?;
        let mut call = CallFunctionOnParams::builder()
            .function_declaration("(el) => el.click()")
            .object_id(oid.clone())
            .await_promise(true)
            .user_gesture(true)
            .return_by_value(true)
            .build()
            .map_err(Error::Invalid)?;
        call.arguments = Some(vec![CallArgument::builder().object_id(oid).build()]);
        tab.send(call).await?;
        Ok(())
    }

    /// Scroll into view and move the cursor to the centre (hover).
    pub async fn mouse_move(&self, tab: &Tab) -> Result<()> {
        self.scroll_into_view(tab).await;
        let (x, y) = self.position(tab).await?.center();
        tab.mouse_move(x, y).await
    }

    /// Scroll into view and press/release at the centre.
    pub async fn mouse_click(&self, tab: &Tab) -> Result<()> {
        self.scroll_into_view(tab).await;
        let (x, y) = self.position(tab).await?.center();
        tab.mouse_click(x, y).await
    }

    /// `outerHTML`.
    pub async fn outer_html(&self, tab: &Tab) -> Result<String> {
        let r = tab
            .send(GetOuterHtmlParams {
                backend_node_id: Some(self.id()),
                ..Default::default()
            })
            .await?;
        Ok(r.outer_html)
    }

    /// Select this `<option>` and fire `change`.
    pub async fn select_option(&self, tab: &Tab) -> Result<()> {
        self.apply(
            tab,
            "(el) => { el.selected = true; el.dispatchEvent(new Event('change', {bubbles: true})); }",
        )
        .await?;
        Ok(())
    }

    /// `DOM.setFileInputFiles`.
    pub async fn send_files(&self, tab: &Tab, paths: &[String]) -> Result<()> {
        tab.send(SetFileInputFilesParams {
            files: paths.to_vec(),
            node_id: None,
            backend_node_id: Some(self.id()),
            object_id: None,
        })
        .await?;
        Ok(())
    }

    /// Screenshot just this element's box to `path` (PNG). Returns the bytes
    /// written.
    pub async fn save_screenshot(&self, tab: &Tab, path: &Path) -> Result<Vec<u8>> {
        let pos = self.position(tab).await?;
        let clip = Viewport {
            x: pos.left,
            y: pos.top,
            width: pos.width(),
            height: pos.height(),
            scale: 1.0,
        };
        let p = CaptureScreenshotParams {
            format: Some(CaptureScreenshotFormat::Png),
            quality: None,
            clip: Some(clip),
            from_surface: None,
            capture_beyond_viewport: None,
            optimize_for_speed: None,
        };
        let r = tab.send(p).await?;
        let bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            r.data.as_ref() as &str,
        )
        .map_err(|e| Error::Image(format!("screenshot base64: {e}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &bytes)?;
        Ok(bytes)
    }

    /// Red overlay for `duration` seconds (0 leaves it on).
    pub async fn highlight_overlay(&self, tab: &Tab, duration: f64) -> Result<()> {
        let cfg = HighlightConfig {
            content_color: Some(Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: Some(0.3),
            }),
            border_color: Some(Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: Some(0.8),
            }),
            ..Default::default()
        };
        tab.send(OverlayEnableParams::default()).await?;
        tab.send(HighlightNodeParams {
            highlight_config: cfg,
            node_id: None,
            backend_node_id: Some(self.id()),
            object_id: None,
            selector: None,
        })
        .await?;
        if duration > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(duration)).await;
            tab.send(HideHighlightParams::default()).await?;
        }
        Ok(())
    }

    /// Scroll into view and left-drag from the centre to `(to_x, to_y)`.
    pub async fn mouse_drag(&self, tab: &Tab, to_x: f64, to_y: f64, steps: usize) -> Result<()> {
        self.scroll_into_view(tab).await;
        let from = self.position(tab).await?.center();
        tab.mouse_drag(from, (to_x, to_y), steps).await
    }
}

// ---------------------------------------------------------------------------
// DOM tree helpers
// ---------------------------------------------------------------------------

/// Concatenated text of a node's text-node descendants (Python `text_all`).
#[must_use]
pub fn text_all(node: &Node) -> String {
    let mut parts = Vec::new();
    collect_text(node, &mut parts);
    parts.join(" ")
}

fn collect_text(node: &Node, out: &mut Vec<String>) {
    if let Some(children) = &node.children {
        for c in children {
            if c.node_type == 3 && !c.node_value.is_empty() {
                out.push(c.node_value.clone());
            }
            if let Some(roots) = &c.shadow_roots {
                if let Some(r) = roots.first() {
                    collect_text(r, out);
                }
            }
            collect_text(c, out);
        }
    }
}

/// Describe a node (full subtree) by backend id.
pub async fn describe(tab: &Tab, backend_node_id: i64, depth: i64) -> Result<Node> {
    let r = tab
        .send(DescribeNodeParams {
            backend_node_id: Some(BackendNodeId::new(backend_node_id)),
            depth: Some(depth),
            pierce: Some(true),
            ..Default::default()
        })
        .await?;
    Ok(r.node)
}

async fn describe_by_node_id(tab: &Tab, node_id: NodeId, depth: i64) -> Result<Node> {
    let r = tab
        .send(DescribeNodeParams {
            node_id: Some(node_id),
            depth: Some(depth),
            pierce: Some(true),
            ..Default::default()
        })
        .await?;
    Ok(r.node)
}

/// A hit from the DOM text search: the element (a text node's parent) and
/// its `text_all`.
#[derive(Debug, Clone)]
pub struct TextHit {
    /// The element.
    pub element: DomElement,
    /// Concatenated descendant text.
    pub text_all: String,
}

/// `DOM.performSearch` for `text` (top document, pierces shadow roots);
/// text-node hits are lifted to their parent element. Python
/// `Tab.find_elements_by_text`.
pub async fn find_elements_by_text(tab: &Tab, text: &str) -> Result<Vec<TextHit>> {
    let text = text.trim();
    // A DOM.getDocument is what makes performSearch see the current tree.
    let document = tab
        .send(
            chromiumoxide_cdp::cdp::browser_protocol::dom::GetDocumentParams {
                depth: Some(-1),
                pierce: Some(true),
            },
        )
        .await?;
    let s = tab
        .send(PerformSearchParams {
            query: text.to_string(),
            include_user_agent_shadow_dom: Some(true),
        })
        .await?;
    let mut ids: Vec<NodeId> = Vec::new();
    if s.result_count > 0 {
        let r = tab
            .send(GetSearchResultsParams {
                search_id: s.search_id.clone(),
                from_index: 0,
                to_index: s.result_count,
            })
            .await?;
        ids = r.node_ids;
    }
    let _ = tab
        .send(DiscardSearchResultsParams {
            search_id: s.search_id,
        })
        .await;
    let mut hits = Vec::new();
    for nid in ids {
        let Ok(node) = describe_by_node_id(tab, nid, -1).await else {
            continue;
        };
        let node = if node.node_type == 3 {
            match node.parent_id {
                Some(pid) => match describe_by_node_id(tab, pid, -1).await {
                    Ok(p) => p,
                    Err(_) => continue,
                },
                // DOM.describeNode may omit parentId. The full document
                // snapshot still identifies the owning element of text hits.
                None => match text_node_parent(&document.root, nid) {
                    Some(parent) => parent.clone(),
                    None => continue,
                },
            }
        } else {
            node
        };
        let mut el = DomElement::new(node.backend_node_id.inner().to_owned());
        el.node_name.clone_from(&node.node_name);
        hits.push(TextHit {
            text_all: text_all(&node),
            element: el,
        });
    }
    Ok(hits)
}

fn text_node_parent(root: &Node, node_id: NodeId) -> Option<&Node> {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if let Some(children) = &node.children {
            for child in children {
                if child.node_id == node_id {
                    return Some(node);
                }
                pending.push(child);
            }
        }
        if let Some(roots) = &node.shadow_roots {
            pending.extend(roots);
        }
        if let Some(document) = &node.content_document {
            pending.push(document);
        }
    }
    None
}

/// First element containing `text`; with `best_match` the one whose
/// `text_all` length is closest to the query. Python `Tab.find_element_by_text`.
pub async fn find_element_by_text(
    tab: &Tab,
    text: &str,
    best_match: bool,
) -> Result<Option<TextHit>> {
    let hits = find_elements_by_text(tab, text).await?;
    if hits.is_empty() {
        return Ok(None);
    }
    if best_match {
        let tl = text.trim().chars().count() as i64;
        return Ok(hits
            .into_iter()
            .min_by_key(|h| (h.text_all.chars().count() as i64 - tl).abs()));
    }
    Ok(hits.into_iter().next())
}

/// Python `Tab.find`: retry [`find_element_by_text`] every 0.5 s until
/// `timeout`.
pub async fn find_with_timeout(
    tab: &Tab,
    text: &str,
    best_match: bool,
    timeout: f64,
) -> Result<Option<TextHit>> {
    let start = tokio::time::Instant::now();
    loop {
        if let Some(h) = find_element_by_text(tab, text, best_match).await? {
            return Ok(Some(h));
        }
        if start.elapsed().as_secs_f64() > timeout {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// All elements matching a CSS selector in the top document (Python
/// `Tab.query_selector_all`). An invalid selector yields an empty list.
pub async fn query_selector_all(tab: &Tab, selector: &str) -> Result<Vec<DomElement>> {
    let doc = tab
        .send(
            chromiumoxide_cdp::cdp::browser_protocol::dom::GetDocumentParams {
                depth: Some(-1),
                pierce: Some(true),
            },
        )
        .await?
        .root;
    let ids = match tab
        .send(QuerySelectorAllParams {
            node_id: doc.node_id,
            selector: selector.trim().to_string(),
        })
        .await
    {
        Ok(r) => r.node_ids,
        Err(Error::Protocol { .. }) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for nid in ids {
        if let Ok(n) = describe_by_node_id(tab, nid, 0).await {
            let mut el = DomElement::new(n.backend_node_id.inner().to_owned());
            el.node_name = n.node_name;
            out.push(el);
        }
    }
    Ok(out)
}
