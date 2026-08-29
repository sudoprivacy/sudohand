//! `download` / `download_link` — port of `core/download.py`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::browser::{
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use serde_json::{json, Value};

use crate::connection::Tab;
use crate::element::query_selector_all;
use crate::elements::{trusted_click, xpath_finder_js};
use crate::Result;

async fn set_download_dir(tab: &Tab, dir: &Path, events: bool) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let abs = std::fs::canonicalize(dir)?;
    tab.send(SetDownloadBehaviorParams {
        behavior: SetDownloadBehaviorBehavior::Allow,
        browser_context_id: None,
        download_path: Some(abs.to_string_lossy().to_string()),
        events_enabled: Some(events),
    })
    .await?;
    Ok(abs)
}

/// Fetch `url` in the page and save it via an anchor click into `path`
/// (a directory; default `./downloads`). `{path, success}`.
pub async fn download(tab: &Tab, url: &str, path: Option<&Path>) -> Result<Value> {
    let dir = match path {
        Some(p) if p.is_dir() || p.extension().is_none() => p.to_path_buf(),
        Some(_) | None => std::env::current_dir()?.join("downloads"),
    };
    set_download_dir(tab, &dir, false).await?;
    let filename = url
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or("")
        .to_string();
    let code = format!(
        r"(elem) => {{
            async function _dl(src, name) {{
                const r = await fetch(src);
                const b = await r.blob();
                const href = URL.createObjectURL(b);
                const a = document.createElement('a');
                a.href = href; a.download = name;
                document.body.appendChild(a); a.click();
                setTimeout(() => {{ document.body.removeChild(a); URL.revokeObjectURL(href); }}, 500);
            }}
            _dl({}, {})
        }}",
        serde_json::to_string(url)?,
        serde_json::to_string(&filename)?
    );
    let bodies = query_selector_all(tab, "body").await?;
    if let Some(body) = bodies.first() {
        body.apply(tab, &code).await?;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    if filename.is_empty() {
        return Ok(json!({"path": Value::Null, "success": false}));
    }
    Ok(json!({"path": filename, "success": true}))
}

/// Trusted-click the XPath-located link, wait for the download to finish,
/// and report where it landed. `{downloaded, xpath, path, filename, bytes}`
/// or `{downloaded: false, xpath, error, clicked?}`.
pub async fn download_link(
    tab: &Tab,
    xpath: &str,
    download_dir: Option<&Path>,
    timeout: f64,
) -> Result<Value> {
    let dir = match download_dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir()?.join("downloads"),
    };
    let abs = set_download_dir(tab, &dir, true).await?;
    let mut rx = tab.subscribe();
    let click = trusted_click(tab, &xpath_finder_js(xpath), "xpath", xpath).await?;
    if click.get("clicked") != Some(&json!(true)) {
        return Ok(json!({
            "downloaded": false,
            "xpath": xpath,
            "error": click.get("error").and_then(Value::as_str).unwrap_or("download link not found"),
        }));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    let mut guid: Option<String> = None;
    let mut filename: Option<String> = None;
    let state: Option<String>;
    let mut file_path: Option<String> = None;
    let mut bytes: Option<u64> = None;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(json!({
                "downloaded": false,
                "xpath": xpath,
                "clicked": true,
                "error": format!("clicked, but no download completed within {timeout}s"),
            }));
        }
        let Ok(Ok(ev)) = tokio::time::timeout(remaining, rx.recv()).await else {
            continue;
        };
        match ev.method.as_str() {
            "Browser.downloadWillBegin" if guid.is_none() => {
                guid = ev
                    .params
                    .get("guid")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                filename = ev
                    .params
                    .get("suggestedFilename")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "Browser.downloadProgress" => {
                let g = ev.params.get("guid").and_then(Value::as_str);
                if guid.as_deref().is_some_and(|mine| Some(mine) != g) {
                    continue;
                }
                let st = ev.params.get("state").and_then(Value::as_str).unwrap_or("");
                if st == "completed" {
                    file_path = ev
                        .params
                        .get("filePath")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    bytes = ev.params.get("receivedBytes").and_then(Value::as_u64);
                    state = Some(st.to_string());
                    break;
                }
                if st == "canceled" {
                    state = Some(st.to_string());
                    break;
                }
            }
            _ => {}
        }
    }
    if state.as_deref() != Some("completed") {
        return Ok(json!({
            "downloaded": false,
            "xpath": xpath,
            "clicked": true,
            "error": format!("download {}", state.unwrap_or_default()),
        }));
    }
    let path = file_path.unwrap_or_else(|| {
        abs.join(filename.clone().unwrap_or_default())
            .to_string_lossy()
            .to_string()
    });
    Ok(json!({
        "downloaded": true,
        "xpath": xpath,
        "path": path,
        "filename": filename,
        "bytes": bytes,
    }))
}
