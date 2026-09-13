//! `cookies_list` / `cookies_save` / `cookies_load` — port of
//! `core/cookies.py` + `connection.CookieJar`. The file format is the JSON
//! array of CDP `Network.Cookie` objects Python writes (`cookies.dat`), so
//! files are interchangeable between the two implementations.

use std::path::{Path, PathBuf};

use chromiumoxide_cdp::cdp::browser_protocol::network::{Cookie, CookieParam};
use chromiumoxide_cdp::cdp::browser_protocol::storage::{GetCookiesParams, SetCookiesParams};
use serde_json::{json, Value};

use crate::connection::Tab;
use crate::{Error, Result};

/// `~/.ai-dev-browser/cookies.dat`.
#[must_use]
pub fn default_cookies_file() -> PathBuf {
    crate::config::base_dir().join("cookies.dat")
}

fn expand(p: Option<&Path>) -> PathBuf {
    match p {
        Some(p) => {
            let s = p.to_string_lossy();
            if let Some(rest) = s.strip_prefix("~/") {
                return crate::config::home_dir().join(rest);
            }
            p.to_path_buf()
        }
        None => default_cookies_file(),
    }
}

/// All browser cookies (browser-level `Storage.getCookies`).
pub async fn get_all(tab: &Tab) -> Result<Vec<Cookie>> {
    let r = tab
        .browser_connection()
        .send(GetCookiesParams::default())
        .await?;
    Ok(r.cookies)
}

/// `{cookies: [{name, domain, path, secure, httpOnly, value(≤50)}], count}`.
pub async fn cookies_list(tab: &Tab, domain: Option<&str>) -> Result<Value> {
    let cookies = get_all(tab).await?;
    let simple: Vec<Value> = cookies
        .iter()
        .filter(|c| domain.is_none_or(|d| c.domain.contains(d)))
        .map(|c| {
            let value: String = c.value.chars().take(50).collect();
            let value = if c.value.chars().count() > 50 {
                format!("{value}...")
            } else {
                value
            };
            json!({
                "name": c.name,
                "domain": c.domain,
                "path": c.path,
                "secure": c.secure,
                "httpOnly": c.http_only,
                "value": value,
            })
        })
        .collect();
    Ok(json!({"cookies": simple, "count": simple.len()}))
}

/// Extract complete live cookies as an array, matching the offline extractor.
/// A session cookie has a null expiry. An empty domain selects all cookies.
pub async fn cookies_extract_live(tab: &Tab, domain: &str) -> Result<Value> {
    let cookies = get_all(tab).await?;
    Ok(Value::Array(
        cookies
            .into_iter()
            .filter(|cookie| domain.is_empty() || cookie.domain.contains(domain))
            .map(|cookie| {
                let expires = if cookie.session || cookie.expires <= 0.0 {
                    None
                } else {
                    Some(cookie.expires)
                };
                json!({
                    "name": cookie.name, "value": cookie.value, "domain": cookie.domain,
                    "path": cookie.path, "secure": cookie.secure, "httpOnly": cookie.http_only,
                    "expires": expires,
                })
            })
            .collect(),
    ))
}

/// Save cookies as JSON; `pattern` is a regex searched in each cookie's
/// JSON (Python `re.search`). `{path, pattern, saved}`.
pub async fn cookies_save(tab: &Tab, path: Option<&Path>, pattern: Option<&str>) -> Result<Value> {
    let path = expand(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cookies = get_all(tab).await?;
    let matcher = match pattern {
        Some(p) => {
            Some(regex_lite::Regex::new(p).map_err(|e| Error::Invalid(format!("pattern: {e}")))?)
        }
        None => None,
    };
    let kept: Vec<Value> = cookies
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .filter(|v| matcher.as_ref().is_none_or(|m| m.is_match(&v.to_string())))
        .collect();
    if !cookies.is_empty() {
        std::fs::write(&path, serde_json::to_string_pretty(&kept)?)?;
    }
    Ok(json!({
        "path": path.to_string_lossy(),
        "pattern": pattern.unwrap_or("all"),
        "saved": true,
    }))
}

/// Load cookies from a JSON file into the browser. `{path, loaded}` or
/// `{error}` when the file is missing.
pub async fn cookies_load(tab: &Tab, path: Option<&Path>) -> Result<Value> {
    let path = expand(path);
    if !path.exists() {
        return Ok(json!({"error": format!("Cookies file not found: {}", path.display())}));
    }
    let raw = std::fs::read_to_string(&path)?;
    let cookies: Vec<Value> = serde_json::from_str(&raw).map_err(|e| {
        Error::Invalid(format!(
            "{}: not a JSON cookie file ({e}); legacy pickle files are not supported",
            path.display()
        ))
    })?;
    let params: Vec<CookieParam> = cookies
        .into_iter()
        .filter_map(|c| cookie_to_param(&c))
        .collect();
    if !params.is_empty() {
        tab.browser_connection()
            .send(SetCookiesParams::new(params))
            .await?;
    }
    Ok(json!({"path": path.to_string_lossy(), "loaded": true}))
}

/// Turn a serialized `Network.Cookie` into a `CookieParam` (drops the
/// read-only fields).
#[must_use]
pub fn cookie_to_param(c: &Value) -> Option<CookieParam> {
    let name = c.get("name")?.as_str()?.to_string();
    let value = c.get("value")?.as_str()?.to_string();
    let mut v = json!({"name": name, "value": value});
    for k in [
        "domain",
        "path",
        "secure",
        "httpOnly",
        "sameSite",
        "expires",
        "priority",
        "sameParty",
        "sourceScheme",
        "sourcePort",
        "partitionKey",
    ] {
        if let Some(x) = c.get(k) {
            if !x.is_null() {
                v[k] = x.clone();
            }
        }
    }
    serde_json::from_value(v).ok()
}
