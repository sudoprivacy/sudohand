//! CDP `Runtime` result → JSON value. Port of `core/_js.py`.
//!
//! `Runtime.evaluate` and `Runtime.callFunctionOn` both answer with
//! `(RemoteObject, ExceptionDetails?)`. Turning that pair into "a value, or
//! an error" lives here once.

use chromiumoxide_cdp::cdp::js_protocol::runtime::{
    DeepSerializedValueType, ExceptionDetails, RemoteObject,
};
use serde_json::{json, Map, Value};

use crate::error::JsEvaluationError;

fn opaque_hint(t: &str) -> &'static str {
    match t {
        "function" => {
            "a function object — it was never called. Wrap it in an IIFE: `(() => { ... })()`"
        }
        "promise" => {
            "a pending promise — pass await_promise=True to resolve it. (Harmless if you called this for its side effect.)"
        }
        "node" => {
            "a DOM node, which cannot cross the CDP boundary. Return a plain value instead (e.g. `el.textContent`, `el.value`), or locate the element with find_by_text / find_by_xpath / html_by_ref"
        }
        "nodelist" => {
            "a DOM NodeList, which cannot cross the CDP boundary. Map it to plain values first, e.g. `[...document.querySelectorAll('a')].map(a => a.href)`"
        }
        "htmlcollection" => {
            "an HTMLCollection, which cannot cross the CDP boundary. Map it to plain values first, e.g. `[...el.children].map(c => c.tagName)`"
        }
        "window" => "a Window object, which cannot cross the CDP boundary",
        "symbol" => "a Symbol, which has no Python representation",
        "error" => {
            "an Error object that was *returned* rather than thrown. Return `err.message` for the text, or `throw` it to fail the call"
        }
        _ => "a value with no Python representation",
    }
}

fn opaque_marker(t: &str) -> Value {
    json!({"__js_type__": t, "hint": format!("expression returned {}", opaque_hint(t))})
}

fn type_name(t: &DeepSerializedValueType) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{t:?}").to_lowercase())
}

/// Read `(type, value)` off a nested deep-serialized node (raw JSON).
fn split(node: &Value) -> (Option<&str>, Option<&Value>) {
    (node.get("type").and_then(Value::as_str), node.get("value"))
}

fn number(v: Option<&Value>) -> Value {
    // CDP sends NaN / Infinity / -0 as strings. JSON has no NaN/Infinity, so
    // those stay as their string spelling; "-0" becomes -0.0.
    match v {
        Some(Value::String(s)) if s == "-0" => json!(-0.0_f64),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

fn from_deep(node: &Value) -> Value {
    let (t, v) = split(node);
    match t {
        Some("undefined" | "null") | None => Value::Null,
        Some("string" | "boolean" | "date" | "regexp") => v.cloned().unwrap_or(Value::Null),
        Some("number") => number(v),
        Some("bigint") => match v {
            Some(Value::String(s)) => s
                .trim_end_matches('n')
                .parse::<i64>()
                .map_or_else(|_| Value::String(s.clone()), Value::from),
            Some(other) => other.clone(),
            None => Value::Null,
        },
        Some("array" | "set") => Value::Array(
            v.and_then(Value::as_array)
                .map(|items| items.iter().map(from_deep).collect())
                .unwrap_or_default(),
        ),
        Some("object" | "map") => {
            let mut out = Map::new();
            if let Some(pairs) = v.and_then(Value::as_array) {
                for pair in pairs {
                    let Some(kv) = pair.as_array() else { continue };
                    let (Some(k), Some(val)) = (kv.first(), kv.get(1)) else {
                        continue;
                    };
                    let key = match k {
                        Value::String(s) => s.clone(),
                        Value::Number(_) | Value::Bool(_) => k.to_string(),
                        other => match from_deep(other) {
                            Value::String(s) => s,
                            Value::Null => "None".to_string(),
                            x => x.to_string(),
                        },
                    };
                    out.insert(key, from_deep(val));
                }
            }
            Value::Object(out)
        }
        Some(other) => opaque_marker(other),
    }
}

fn from_exception_details(exc: &ExceptionDetails, expression: Option<&str>) -> JsEvaluationError {
    let message = exc
        .exception
        .as_ref()
        .and_then(|e| e.description.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if exc.text.is_empty() {
                "JavaScript error".to_string()
            } else {
                exc.text.clone()
            }
        });
    JsEvaluationError {
        message,
        expression: expression.map(str::to_string),
        console: Vec::new(),
    }
}

/// Turn a CDP `(RemoteObject, ExceptionDetails?)` pair into a JSON value.
///
/// # Errors
/// The expression threw.
pub fn unwrap(
    remote: &RemoteObject,
    exception: Option<&ExceptionDetails>,
    expression: Option<&str>,
) -> Result<Value, JsEvaluationError> {
    if let Some(exc) = exception {
        return Err(from_exception_details(exc, expression));
    }
    if let Some(deep) = &remote.deep_serialized_value {
        // The outer node is typed; nested ones are raw JSON. Normalise to raw
        // once so one recursion handles both.
        let node = json!({
            "type": type_name(&deep.r#type),
            "value": deep.value.clone().unwrap_or(Value::Null),
        });
        return Ok(from_deep(&node));
    }
    Ok(remote.value.clone().unwrap_or(Value::Null))
}

/// Stringify one console argument the way `js_evaluate` does.
#[must_use]
pub fn stringify_arg(arg: &RemoteObject) -> String {
    if let Some(v) = &arg.value {
        return match v {
            Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| s.clone()),
            other => other.to_string(),
        };
    }
    if let Some(d) = &arg.description {
        return d.clone();
    }
    if let Some(u) = &arg.unserializable_value {
        return u.inner().clone();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_object_unwraps() {
        let node = json!({"type":"object","value":[["a",{"type":"number","value":1}],["b",{"type":"array","value":[{"type":"string","value":"x"}]}]]});
        assert_eq!(from_deep(&node), json!({"a":1,"b":["x"]}));
    }

    #[test]
    fn opaque_marker_for_node() {
        let node = json!({"type":"node","value":null});
        let v = from_deep(&node);
        assert_eq!(v["__js_type__"], "node");
    }

    #[test]
    fn special_numbers() {
        assert_eq!(
            from_deep(&json!({"type":"number","value":"NaN"})),
            json!("NaN")
        );
        assert_eq!(from_deep(&json!({"type":"number","value":2.5})), json!(2.5));
    }
}
