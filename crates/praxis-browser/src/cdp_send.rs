//! `cdp_send` — raw-protocol escape hatch. Port of `core/cdp.py`.

use serde_json::{json, Map, Value};

use crate::connection::Tab;
use crate::{Error, Result};

/// `snake_case` → `camelCase` (the CDP wire form). Keys already camelCase
/// pass through unchanged.
#[must_use]
pub fn snake_to_camel(k: &str) -> String {
    let mut out = String::with_capacity(k.len());
    let mut upper = false;
    for ch in k.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Send `method` (`Domain.command`) with JSON `params` on the tab session and
/// return `{result}` verbatim. Parameter keys may be snake_case or camelCase.
pub async fn cdp_send(tab: &Tab, method: &str, params: Option<&str>) -> Result<Value> {
    if method.split('.').count() != 2 {
        return Err(Error::Invalid(format!(
            "method must be `Domain.command`, e.g. Page.navigate (got {method:?})"
        )));
    }
    let params: Value = match params {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str(raw)?,
        _ => json!({}),
    };
    let params = match params {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (snake_to_camel(&k), v))
                .collect::<Map<_, _>>(),
        ),
        other => {
            return Err(Error::Invalid(format!(
                "params must be a JSON object (got {other})"
            )))
        }
    };
    let result = tab.connection().send_raw(method, params).await?;
    Ok(json!({"result": result}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel() {
        assert_eq!(snake_to_camel("device_scale_factor"), "deviceScaleFactor");
        assert_eq!(snake_to_camel("deviceScaleFactor"), "deviceScaleFactor");
        assert_eq!(snake_to_camel("x"), "x");
    }
}
