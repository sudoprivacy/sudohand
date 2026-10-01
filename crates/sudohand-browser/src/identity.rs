//! Per-session identity overrides, persisted in the GUID-validated registry.
use serde_json::{json, Map, Value};
use std::time::Duration;

use crate::{
    browser::StartOptions,
    config,
    connection::{BrowserClient, Tab},
};

fn geo(value: &str) -> Option<Value> {
    let (lat, lon) = value.split_once(',')?;
    let lat: f64 = lat.trim().parse().ok()?;
    let lon: f64 = lon.trim().parse().ok()?;
    (lat.is_finite() && lon.is_finite()).then(|| json!([lat, lon]))
}

fn should_match(opts: &StartOptions) -> bool {
    if opts.timezone.as_ref().is_some_and(|s| !s.is_empty())
        || opts.geo.as_ref().is_some_and(|s| !s.is_empty())
    {
        return false;
    }
    opts.match_proxy
        .or_else(|| {
            match std::env::var("AI_DEV_BROWSER_MATCH_PROXY")
                .ok()?
                .to_lowercase()
                .as_str()
            {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            }
        })
        .unwrap_or_else(|| {
            opts.extra_args
                .iter()
                .any(|arg| arg.starts_with("--proxy-server="))
        })
}

fn parse_location(raw: &str) -> Option<Value> {
    let data: Value = serde_json::from_str(raw).ok()?;
    let timezone = data["timezone"].as_str().filter(|s| !s.is_empty())?;
    let mut record = json!({"timezone": timezone});
    if let Some(ip) = data["query"].as_str().or_else(|| data["ip"].as_str()) {
        record["egress_ip"] = json!(ip);
    }
    let lat = data["lat"].as_f64().or_else(|| data["latitude"].as_f64());
    let lon = data["lon"].as_f64().or_else(|| data["longitude"].as_f64());
    if let (Some(lat), Some(lon)) = (lat, lon) {
        record["geo"] = json!([lat, lon]);
    } else if let Some(pair) = data["loc"].as_str().and_then(geo) {
        record["geo"] = pair;
    }
    Some(record)
}

async fn derive(port: u16) -> Option<Value> {
    let endpoint = std::env::var("AI_DEV_BROWSER_GEO_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://ip-api.com/json".into());
    let mut browser = BrowserClient::connect(config::DEFAULT_DEBUG_HOST, port)
        .await
        .ok()?;
    // A browser navigation deliberately uses Chrome's proxy, not the host HTTP client.
    let tab = browser.new_tab(&endpoint).await.ok()?;
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(raw) = tab
                .evaluate("document.body && document.body.innerText")
                .await
            {
                if let Some(location) = raw.as_str().and_then(parse_location) {
                    return location;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .ok();
    let _ = tab.close_target().await;
    result
}

pub(crate) async fn configure(port: u16, opts: &StartOptions, out: &mut Map<String, Value>) {
    let mut identity = Map::new();
    let mut failed_lookup = false;
    if should_match(opts) {
        if let Some(Value::Object(derived)) = derive(port).await {
            identity = derived;
        } else {
            failed_lookup = true;
            out.insert("identity_consistent".into(), json!(false));
            out.insert("identity_warning".into(), json!("proxy set but egress geo-lookup failed — timezone/geolocation left at host values, so the IP and location signals may be INCONSISTENT. Pass timezone=/geo= explicitly, verify the proxy, or set AI_DEV_BROWSER_GEO_ENDPOINT."));
        }
    }
    for (name, value) in [("timezone", &opts.timezone), ("locale", &opts.locale)] {
        if let Some(value) = value.as_ref().filter(|s| !s.is_empty()) {
            identity.insert(name.into(), json!(value));
        }
    }
    if let Some(pair) = opts.geo.as_deref().and_then(geo) {
        identity.insert("geo".into(), pair);
    }
    if identity.is_empty() {
        return;
    }
    if !failed_lookup {
        out.insert("identity_consistent".into(), json!(true));
        for (key, value) in &identity {
            out.insert(
                if key == "geo" { "geolocation" } else { key }.into(),
                value.clone(),
            );
        }
    }
    let saved = async {
        let ws = crate::cdp::http::ws_debugger_url(
            config::DEFAULT_DEBUG_HOST,
            port,
            Duration::from_secs(5),
        )
        .await?;
        let mut record = crate::registry::lookup(port, &ws)
            .ok_or_else(|| crate::Error::Invalid("instance registry is unavailable".into()))?;
        record["identity"] = Value::Object(identity);
        crate::registry::write(port, &record)
    }
    .await;
    if let Err(error) = saved {
        out.insert("identity_consistent".into(), json!(false));
        out.insert(
            "identity_warning".into(),
            json!(format!("Cannot persist browser identity: {error}")),
        );
    }
}

pub(crate) async fn apply(tab: &Tab, identity: &Value) {
    for (key, method, parameter) in [
        ("timezone", "Emulation.setTimezoneOverride", "timezoneId"),
        ("locale", "Emulation.setLocaleOverride", "locale"),
    ] {
        if let Some(value) = identity[key].as_str() {
            let _ = tab
                .connection()
                .send_raw(method, json!({parameter: value}))
                .await;
        }
    }
    if let Some(pair) = identity["geo"].as_array().filter(|v| v.len() == 2) {
        let _ = tab
            .connection()
            .send_raw(
                "Emulation.setGeolocationOverride",
                json!({"latitude": pair[0], "longitude": pair[1], "accuracy": 50}),
            )
            .await;
        let _ = tab
            .browser_connection()
            .send_raw(
                "Browser.setPermission",
                json!({"permission": {"name": "geolocation"}, "setting": "granted"}),
            )
            .await;
    }
}
