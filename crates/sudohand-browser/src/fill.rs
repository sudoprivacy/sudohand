//! Verified replacement of an editable field, shared by both typing locators.

use chromiumoxide_cdp::cdp::browser_protocol::input::InsertTextParams;
use serde_json::{json, Map, Value};

use crate::{actions::dispatch_key, connection::Tab, element::DomElement, human, Result};

async fn keys(tab: &Tab, text: &str) -> Result<()> {
    for ch in text.chars() {
        let key = ch.to_string();
        let (code, virtual_key) = if ch.is_ascii_digit() {
            (format!("Digit{ch}"), i64::from(u32::from(ch)))
        } else if ch.is_ascii_alphabetic() {
            let upper = ch.to_ascii_uppercase();
            (format!("Key{upper}"), i64::from(u32::from(upper)))
        } else {
            (String::new(), 0)
        };
        dispatch_key(tab, &key, &code, virtual_key, 0, Some(&key)).await?;
    }
    Ok(())
}

/// Each attempt replaces the whole field. `clear` is retained on public options
/// for compatibility; it no longer changes the target value into an append.
pub(crate) async fn fill(
    tab: &Tab,
    element: &DomElement,
    text: &str,
    keystrokes: bool,
    human_like: bool,
) -> Map<String, Value> {
    let methods = if keystrokes {
        ["keys", "insertText", "native"]
    } else if human_like {
        ["human", "keys", "native"]
    } else {
        ["insertText", "keys", "native"]
    };
    let encoded = json!(text).to_string();
    // Compare in the page; do not return the contents of a potentially secret field.
    let readback = format!(
        "field => {{ const value = field.value ?? field.textContent ?? ''; return {{matches: value === {encoded}, empty: value.length === 0}}; }}"
    );
    let native = format!(
        "field => {{ const realm = field.ownerDocument.defaultView; const prototype = field instanceof realm.HTMLTextAreaElement ? realm.HTMLTextAreaElement.prototype : realm.HTMLInputElement.prototype; const setter = Object.getOwnPropertyDescriptor(prototype, 'value')?.set; if (setter) setter.call(field, {encoded}); else field.value = {encoded}; for (const name of ['input', 'change']) field.dispatchEvent(new realm.Event(name, {{bubbles: true}})); }}"
    );
    let mut tried = Vec::new();
    let mut empty = true;
    for method in methods {
        let _ = element.apply(tab, "field => { field.focus?.(); if ('value' in field) { field.value = ''; field.dispatchEvent(new Event('input', {bubbles: true})); } else if (field.isContentEditable) field.textContent = ''; }").await;
        let _ = element.focus(tab).await;
        tried.push(method);
        let attempt = match method {
            "insertText" => tab.send(InsertTextParams::new(text)).await.map(|_| ()),
            "keys" => keys(tab, text).await,
            "human" => human::type_text(tab, text, Some(true)).await,
            _ => element.apply(tab, &native).await.map(|_| ()),
        };
        if attempt.is_err() {
            continue;
        }
        let state = element.apply(tab, &readback).await.unwrap_or(Value::Null);
        empty = state["empty"].as_bool().unwrap_or(true);
        if state["matches"] == true {
            return json!({"typed": true, "verified": true, "method": method, "methods_tried": tried})
                .as_object().expect("object literal").clone();
        }
    }
    json!({"typed": !empty, "verified": false, "method": null, "methods_tried": tried})
        .as_object()
        .expect("object literal")
        .clone()
}
