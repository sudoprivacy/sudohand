//! `download` / `download_link` — port of `core/download.py`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::browser::{
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use serde_json::{json, Value};

use crate::connection::Tab;
use crate::elements::{trusted_click, xpath_finder_js};
use crate::{Error, Result};

async fn set_download_dir(tab: &Tab, dir: &Path, events: bool) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let abs = std::fs::canonicalize(dir)?;
    // Windows canonicalize returns an extended-length path. Chrome's download
    // directory interface expects normal drive/UNC syntax, as Python resolve
    // supplies; passing the verbatim prefix can silently discard the download.
    #[cfg(windows)]
    let abs = PathBuf::from(windows_download_path(&abs.to_string_lossy()));
    tab.send(SetDownloadBehaviorParams {
        behavior: SetDownloadBehaviorBehavior::Allow,
        browser_context_id: None,
        download_path: Some(abs.to_string_lossy().to_string()),
        events_enabled: Some(events),
    })
    .await?;
    Ok(abs)
}

#[cfg(any(windows, test))]
fn windows_download_path(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    if let Some(drive) = path.strip_prefix(r"\\?\") {
        let bytes = drive.as_bytes();
        if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1..3] == *b":\\" {
            return drive.into();
        }
    }
    path.into()
}

#[doc = include_str!("../help/download.md")]
pub async fn download(tab: &Tab, url: &str, path: Option<&Path>) -> Result<Value> {
    let dir = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?.join("downloads"),
    };
    let abs = set_download_dir(tab, &dir, true).await?;
    // Subscribe before acting and keep this CDP connection alive until Chrome
    // finishes. Disconnecting while fetch is pending resets the download folder.
    let mut rx = tab.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let code = format!(
        r"(async () => {{
            const controller = new AbortController();
            const timer = setTimeout(() => controller.abort(), 25000);
            try {{
                const src = new URL({}, location.href);
                const name = decodeURIComponent(src.pathname.split('/').pop()) || 'download';
                const r = await fetch(src, {{signal: controller.signal}});
                if (!r.ok) throw new Error('Download HTTP ' + r.status);
                const b = await r.blob();
                const href = URL.createObjectURL(b);
                const a = document.createElement('a');
                a.href = href; a.download = name;
                document.body.appendChild(a); a.click();
                a.remove();
                return {{href, bytes: b.size}};
            }} catch (error) {{
                // Normalize DOMException (AbortError / CORS) for CDP's deep
                // serialization, whose protocol enum cannot decode platformobject.
                throw new Error(String(error));
            }} finally {{ clearTimeout(timer); }}
        }})()",
        serde_json::to_string(url)?,
    );
    let started = tab
        .evaluate_opts(&code, true, true)
        .await
        .map_err(|e| Error::Download(e.to_string()))?;
    let href = started["href"]
        .as_str()
        .ok_or_else(|| Error::Download("page did not return a download URL".into()))?;
    let result = async {
        let mut guid = None;
        let mut filename = None;
        loop {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .map_err(|_| Error::Download("download did not complete within 30s".into()))?
                .map_err(|e| Error::Download(format!("download event stream lost: {e}")))?;
            match ev.method.as_str() {
                "Browser.downloadWillBegin" if ev.params["url"].as_str() == Some(href) => {
                    guid = ev.params["guid"].as_str().map(str::to_owned);
                    filename = ev.params["suggestedFilename"].as_str().map(str::to_owned);
                }
                "Browser.downloadProgress"
                    if guid
                        .as_deref()
                        .is_some_and(|g| ev.params["guid"].as_str() == Some(g)) =>
                {
                    match ev.params["state"].as_str() {
                        Some("completed") => {
                            let filename = filename.as_deref().ok_or_else(|| {
                                Error::Download("completed download has no filename".into())
                            })?;
                            let saved = ev.params["filePath"]
                                .as_str()
                                .map_or_else(|| abs.join(filename), PathBuf::from);
                            let metadata = std::fs::metadata(&saved).map_err(|e| {
                                Error::Download(format!("completed file is unavailable: {e}"))
                            })?;
                            if !metadata.is_file()
                                || Some(metadata.len()) != started["bytes"].as_u64()
                            {
                                return Err(Error::Download(
                                    "completed file size differs from the response".into(),
                                ));
                            }
                            return Ok(
                                json!({"success": true, "path": saved, "filename": filename,
                                "bytes": metadata.len()}),
                            );
                        }
                        Some("canceled") => {
                            return Err(Error::Download("Chrome canceled the download".into()))
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    .await;
    // Revoke only after the transfer settles. Cleanup never replays the click.
    let _ = tab
        .evaluate(&format!(
            "URL.revokeObjectURL({})",
            serde_json::to_string(href)?
        ))
        .await;
    result
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

#[cfg(test)]
mod path_tests {
    use super::windows_download_path;

    #[test]
    fn chrome_receives_normal_windows_drive_and_unc_paths() {
        for (input, expected) in [
            (r"\\?\C:\Users\fixture\下载", r"C:\Users\fixture\下载"),
            (
                r"\\?\UNC\server\share\downloads",
                r"\\server\share\downloads",
            ),
            (r"C:\downloads", r"C:\downloads"),
            (
                r"\\?\Volume{fixture}\downloads",
                r"\\?\Volume{fixture}\downloads",
            ),
        ] {
            assert_eq!(windows_download_path(input), expected);
        }
    }
}
