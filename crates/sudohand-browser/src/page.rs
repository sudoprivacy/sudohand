//! Page tools: `page_goto`, `page_reload`, `page_wait_ready`, `page_wait_url`,
//! `page_info`, `page_html`, `page_screenshot`, `page_pdf`, `js_evaluate`.
//! Port of `core/page.py` + `core/navigation.py`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide_cdp::cdp::js_protocol::runtime::{
    EnableParams as RuntimeEnableParams, RemoteObject,
};
use serde_json::{json, Map, Value};

use crate::config::resolve_output_dir;
use crate::connection::Tab;
use crate::error::ConsoleEntry;
use crate::geometry::{
    self, ScreenshotMeta, MAX_SCREENSHOT_LONG_EDGE, MAX_SCREENSHOT_TOTAL_PIXELS,
};
use crate::image_cap::{self, ImageCap};
use crate::{Error, Result};

/// Navigate and (optionally) wait for `document.readyState === "complete"`.
/// Returns `{url, title, success}`.
pub async fn page_goto(tab: &Tab, url: &str, wait: bool) -> Result<Value> {
    tab.navigate(url).await?;
    if wait {
        page_wait_ready(tab, 30.0, 0.5).await;
    }
    let title = tab
        .evaluate("document.title")
        .await
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let current = tab.current_url().await;
    Ok(json!({"url": current, "title": title, "success": true}))
}

/// Block until `document.readyState === "complete"` (+ `idle_time`), or
/// `timeout` seconds. Returns `true` on load, `false` on timeout.
pub async fn page_wait_ready(tab: &Tab, timeout: f64, idle_time: f64) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed().as_secs_f64() < timeout {
        if let Ok(Value::String(s)) = tab.evaluate("document.readyState").await {
            if s == "complete" {
                tokio::time::sleep(Duration::from_secs_f64(idle_time.max(0.0))).await;
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// Whole-document HTML: `{html, length}` (length in characters).
pub async fn page_html(tab: &Tab, outer: bool) -> Result<Value> {
    let expr = if outer {
        "document.documentElement.outerHTML"
    } else {
        "document.documentElement.innerHTML"
    };
    let v = tab.evaluate(expr).await?;
    let html = v.as_str().unwrap_or("").to_string();
    let length = html.chars().count();
    Ok(json!({"html": html, "length": length}))
}

/// Options for [`page_screenshot`].
#[derive(Debug, Clone)]
pub struct ScreenshotOptions {
    /// Output path; default `$AI_DEV_BROWSER_OUTPUT_DIR|./output/{timestamp}.png`.
    pub path: Option<PathBuf>,
    /// Capture beyond the viewport.
    pub full_page: bool,
    /// Resize so image px map to CSS px (DPR) and fit the caps.
    pub css_scale: bool,
    /// Long-edge cap (0 disables).
    pub max_long_edge: u32,
    /// Total-pixel cap (0 disables).
    pub max_total_pixels: u64,
    /// Per-call byte / dimension cap (overrides the two caps above; falls
    /// back to `AI_DEV_BROWSER_IMAGE_CAP_*` when `None`).
    pub image_cap: Option<ImageCap>,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            path: None,
            full_page: false,
            css_scale: true,
            max_long_edge: MAX_SCREENSHOT_LONG_EDGE,
            max_total_pixels: MAX_SCREENSHOT_TOTAL_PIXELS,
            image_cap: None,
        }
    }
}

pub(crate) fn timestamp_name(ext: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let micros = now.subsec_micros();
    // Civil date from epoch seconds (UTC) — no chrono dependency.
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}{m:02}{d:02}_{:02}{:02}{:02}_{micros:06}.{ext}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Capture a screenshot, scale it for LLM vision, embed the coordinate
/// metadata, and return `{path, size, width, height, scale_factor, device_pixel_ratio}`.
pub async fn page_screenshot(tab: &Tab, opts: &ScreenshotOptions) -> Result<Value> {
    let path = if let Some(p) = &opts.path {
        p.clone()
    } else {
        let dir = resolve_output_dir();
        std::fs::create_dir_all(&dir)?;
        dir.join(timestamp_name("png"))
    };
    let vp = tab
        .evaluate(
            "({width: window.innerWidth, height: window.innerHeight, devicePixelRatio: window.devicePixelRatio})",
        )
        .await?;
    let vw = vp.get("width").and_then(Value::as_f64).unwrap_or(0.0);
    let vh = vp.get("height").and_then(Value::as_f64).unwrap_or(0.0);
    let dpr = vp
        .get("devicePixelRatio")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);

    let png = tab.capture_png(opts.full_page).await?;
    let img = geometry::decode_png(&png)?;
    let (ow, oh) = img.dimensions();
    let image_cap = image_cap::resolve_cap(opts.image_cap.clone());

    let mut path = path;
    let mut cap_result = None;
    let (width, height, scale_factor) = if opts.css_scale {
        if let Some(cap) = &image_cap {
            // DPR normalisation first, then hand off to the cap (which owns
            // the resize / JPEG search and may change the extension).
            let (tw, th) = geometry::fit_dimensions(ow, oh, dpr, 0, 0);
            let resized = geometry::resize(&img, tw.max(1), th.max(1));
            geometry::write_png(&path, &resized)?;
            let r = image_cap::apply_image_cap(&path, cap, true)?;
            path.clone_from(&r.final_path);
            let (w, h) = (r.final_width, r.final_height);
            cap_result = Some(r);
            (w, h, if w > 0 { vw / f64::from(w) } else { 1.0 })
        } else {
            let (tw, th) =
                geometry::fit_dimensions(ow, oh, dpr, opts.max_long_edge, opts.max_total_pixels);
            let resized = geometry::resize(&img, tw.max(1), th.max(1));
            geometry::write_png(&path, &resized)?;
            (tw, th, if tw > 0 { vw / f64::from(tw) } else { 1.0 })
        }
    } else {
        geometry::write_png(&path, &img)?;
        (ow, oh, 1.0)
    };
    let scale_factor = (scale_factor * 1e6).round() / 1e6;
    let meta = ScreenshotMeta {
        scale_factor,
        viewport_width: vw as u32,
        viewport_height: vh as u32,
        image_width: width,
        image_height: height,
        device_pixel_ratio: dpr,
    };
    image_cap::write_metadata(&path, &meta)?;
    let size = std::fs::metadata(&path)?.len();
    let mut out = Map::new();
    out.insert("path".into(), json!(path.to_string_lossy()));
    out.insert("size".into(), json!(size));
    out.insert("width".into(), json!(width));
    out.insert("height".into(), json!(height));
    out.insert("scale_factor".into(), json!(scale_factor));
    out.insert("device_pixel_ratio".into(), json!(dpr));
    if let Some(r) = cap_result {
        out.insert("format".into(), json!(r.format));
        out.insert("capped".into(), json!(r.capped));
    }
    Ok(Value::Object(out))
}

/// `{url, title, ready, state}` without a full discover.
pub async fn page_info(tab: &Tab) -> Result<Value> {
    let state = tab
        .evaluate("document.readyState")
        .await
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    let url = tab.current_url().await;
    let title = tab
        .evaluate("document.title")
        .await
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| tab.target.title.clone());
    Ok(json!({"url": url, "title": title, "ready": state == "complete", "state": state}))
}

/// `Page.reload`. `{success}`.
pub async fn page_reload(tab: &Tab, ignore_cache: bool) -> Result<Value> {
    tab.reload(ignore_cache).await?;
    Ok(json!({"success": true}))
}

/// Block until the top-level URL matches `exact`, or contains / regex-matches
/// `pattern`. `{matched, url, elapsed}`.
pub async fn page_wait_url(
    tab: &Tab,
    pattern: Option<&str>,
    exact: Option<&str>,
    timeout: f64,
) -> Result<Value> {
    let pattern = pattern.filter(|value| !value.is_empty());
    let exact = exact.filter(|value| !value.is_empty());
    let start = tokio::time::Instant::now();
    loop {
        let elapsed = start.elapsed().as_secs_f64();
        let url = tab.current_url().await;
        let rounded = (elapsed * 100.0).round() / 100.0;
        if elapsed > timeout {
            return Ok(json!({"matched": false, "url": url, "elapsed": rounded}));
        }
        let matched = match (exact, pattern) {
            (Some(e), _) => url == e,
            (None, Some(p)) => {
                url.contains(p)
                    || fancy_regex::Regex::new(p)
                        .map_err(|error| Error::Invalid(error.to_string()))?
                        .is_match(&url)
                        .map_err(|error| Error::Invalid(format!("pattern: {error}")))?
            }
            (None, None) => false,
        };
        if matched {
            return Ok(json!({"matched": true, "url": url, "elapsed": rounded}));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Options for [`page_pdf`] (`Page.printToPDF`).
#[derive(Debug, Clone)]
pub struct PdfOptions {
    /// Output path; default `$AI_DEV_BROWSER_OUTPUT_DIR|./output/{timestamp}.pdf`.
    pub path: Option<PathBuf>,
    /// Landscape orientation.
    pub landscape: bool,
    /// Print background graphics.
    pub print_background: bool,
    /// Rendering scale.
    pub scale: f64,
    /// Paper width (inches).
    pub paper_width: f64,
    /// Paper height (inches).
    pub paper_height: f64,
    /// Margins (inches).
    pub margin_top: f64,
    /// Bottom margin.
    pub margin_bottom: f64,
    /// Left margin.
    pub margin_left: f64,
    /// Right margin.
    pub margin_right: f64,
    /// `"1-5"`, `"1,3,5-9"`; empty = all.
    pub page_ranges: String,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            path: None,
            landscape: false,
            print_background: true,
            scale: 1.0,
            paper_width: 8.5,
            paper_height: 11.0,
            margin_top: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            margin_right: 0.0,
            page_ranges: String::new(),
        }
    }
}

fn count_pdf_pages(bytes: &[u8]) -> usize {
    // /Type /Page (leaf) minus /Type /Pages (tree node), best effort.
    let re_page = regex_lite::Regex::new(r"/Type\s*/Page\b").expect("static regex");
    let re_pages = regex_lite::Regex::new(r"/Type\s*/Pages\b").expect("static regex");
    let text = String::from_utf8_lossy(bytes);
    let pages = re_page.find_iter(&text).count();
    let tree = re_pages.find_iter(&text).count();
    pages.saturating_sub(tree).max(1)
}

/// Print the page to PDF. `{path, size, pages}`. Headed Chrome cannot print
/// ("PrintToPDF is not available") — use `browser_start --headless`.
pub async fn page_pdf(tab: &Tab, opts: &PdfOptions) -> Result<Value> {
    use chromiumoxide_cdp::cdp::browser_protocol::page::PrintToPdfParams;
    let path = if let Some(p) = &opts.path {
        p.clone()
    } else {
        let dir = resolve_output_dir();
        std::fs::create_dir_all(&dir)?;
        dir.join(timestamp_name("pdf"))
    };
    let p = PrintToPdfParams {
        landscape: Some(opts.landscape),
        display_header_footer: None,
        print_background: Some(opts.print_background),
        scale: Some(opts.scale),
        paper_width: Some(opts.paper_width),
        paper_height: Some(opts.paper_height),
        margin_top: Some(opts.margin_top),
        margin_bottom: Some(opts.margin_bottom),
        margin_left: Some(opts.margin_left),
        margin_right: Some(opts.margin_right),
        page_ranges: (!opts.page_ranges.is_empty()).then(|| opts.page_ranges.clone()),
        header_template: None,
        footer_template: None,
        prefer_css_page_size: Some(false),
        transfer_mode: None,
        generate_tagged_pdf: None,
        generate_document_outline: None,
    };
    let r = tab.send(p).await?;
    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        r.data.as_ref() as &str,
    )
    .map_err(|e| Error::Image(format!("pdf base64: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &bytes)?;
    Ok(json!({
        "path": path.to_string_lossy(),
        "size": bytes.len(),
        "pages": count_pdf_pages(&bytes),
    }))
}

/// Read the scaling metadata back from a screenshot (PNG or JPEG) produced
/// by either implementation.
pub fn read_screenshot_metadata(path: &Path) -> Result<Option<ScreenshotMeta>> {
    image_cap::read_metadata(path)
}

fn console_text(args: &[RemoteObject]) -> String {
    args.iter()
        .map(crate::js::stringify_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Evaluate arbitrary JS with console capture and navigation feedback:
/// `{result, url_before, url_after, title_after, navigated, console?}`.
/// A page-side throw is an error (with the console trail attached).
pub async fn js_evaluate(tab: &Tab, expression: &str) -> Result<Value> {
    js_evaluate_in(tab, expression, None).await
}

/// [`js_evaluate`] with an optional cross-origin iframe (`frame`: URL
/// substring or target id) whose own CDP session runs the expression.
pub async fn js_evaluate_in(tab: &Tab, expression: &str, frame: Option<&str>) -> Result<Value> {
    let session = match frame {
        Some(f) => Some(tab.frame_session(f).await?),
        None => None,
    };
    let before = tab
        .evaluate("({url: window.location.href, title: document.title})")
        .await?;
    let url_before = before
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    match &session {
        Some(sid) => {
            tab.connection()
                .send_with(
                    RuntimeEnableParams::default(),
                    crate::cdp::COMMAND_TIMEOUT,
                    Some(sid),
                )
                .await?;
        }
        None => {
            tab.send(RuntimeEnableParams::default()).await?;
        }
    }
    let mut rx = tab.subscribe();
    let eval = match &session {
        Some(sid) => tab.evaluate_in_session(expression, sid).await,
        None => tab.evaluate(expression).await,
    };
    // Drain console events that arrived during the eval.
    let mut console: Vec<ConsoleEntry> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if ev.method == "Runtime.consoleAPICalled" {
            let level = ev
                .params
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("log")
                .to_string();
            let args: Vec<RemoteObject> = ev
                .params
                .get("args")
                .cloned()
                .and_then(|a| serde_json::from_value(a).ok())
                .unwrap_or_default();
            console.push(ConsoleEntry {
                level,
                text: console_text(&args),
            });
        }
    }
    let result = match eval {
        Ok(v) => v,
        Err(Error::JsEvaluation(mut e)) => {
            e.console = console;
            return Err(Error::JsEvaluation(e));
        }
        Err(e) => return Err(e),
    };

    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = tab
        .evaluate("({url: window.location.href, title: document.title})")
        .await
        .ok();
    let url_after = after
        .as_ref()
        .and_then(|a| a.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let title_after = after
        .as_ref()
        .and_then(|a| a.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let navigated = !url_before.is_empty() && url_after.as_deref().is_some_and(|u| u != url_before);

    let mut out = Map::new();
    out.insert("result".into(), result);
    out.insert("url_before".into(), json!(url_before));
    out.insert("url_after".into(), json!(url_after));
    out.insert("title_after".into(), json!(title_after));
    out.insert("navigated".into(), json!(navigated));
    if !console.is_empty() {
        out.insert("console".into(), json!(console));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }
}
