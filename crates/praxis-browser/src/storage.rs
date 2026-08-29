//! `storage_get` / `storage_set` — `localStorage` via `DOMStorage`. Port of
//! `core/storage.py` (whose Python body calls a `Tab.get_local_storage` that
//! does not exist — the intent is `Tab.storage_get`, which is what this does).

use serde_json::{json, Map, Value};

use crate::connection::Tab;
use crate::Result;

/// `{key, value}` for one key, or `{items, count}` for all.
pub async fn storage_get(tab: &Tab, key: Option<&str>) -> Result<Value> {
    let items = tab.storage_get().await?;
    Ok(match key {
        Some(k) => json!({"key": k, "value": items.get(k).cloned().unwrap_or(Value::Null)}),
        None => json!({"count": items.len(), "items": items}),
    })
}

/// Batch (`items`) or single (`key` + `value`) set. `{set}` / `{key, value}`.
pub async fn storage_set(
    tab: &Tab,
    items: Option<&Map<String, Value>>,
    key: Option<&str>,
    value: Option<&str>,
) -> Result<Value> {
    if let (Some(k), Some(v)) = (key, value) {
        let mut m = Map::new();
        m.insert(k.to_string(), json!(v));
        tab.storage_set(&m).await?;
        return Ok(json!({"key": k, "value": v}));
    }
    if let Some(items) = items.filter(|m| !m.is_empty()) {
        tab.storage_set(items).await?;
        return Ok(json!({"set": items.len()}));
    }
    Ok(json!({"error": "Must specify items dict or key/value pair"}))
}
