//! Workflow leaf steps and their executor. A [`Step`] is one browser action
//! expressed with the same locators the tools use (accessible name, XPath,
//! html id, URL substring); [`Runner::step`] runs it on one tab with
//! `{{name}}` placeholders substituted from the run's variables.
//!
//! Steps are composed into graphs in code — see [`crate::flow`] — and
//! looked up by name through [`crate::registry`]. Browser-only by design.

// The ported adb modules are clippy-pedantic; the flow layer follows the
// workspace's standard clippy level like praxis-desktop's.
#![allow(clippy::pedantic)]

use crate::connection::{connect_browser, get_active_tab, BrowserClient, Tab};
use crate::elements::TypeByTextOptions;
use crate::tools;
use praxis_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Navigate the tab.
    Goto {
        url: String,
        #[serde(default = "yes")]
        wait: bool,
    },
    /// Click the element with this accessible name / text.
    ClickText {
        text: String,
        #[serde(default = "five")]
        timeout: f64,
    },
    ClickXpath {
        xpath: String,
    },
    ClickHtmlId {
        html_id: String,
    },
    /// Type into the input with this accessible name.
    TypeText {
        name: String,
        text: String,
        #[serde(default)]
        clear: bool,
        #[serde(default)]
        enter: bool,
        #[serde(default = "five")]
        timeout: f64,
    },
    PressKey {
        key: String,
    },
    /// Wait until the URL contains `pattern`.
    WaitUrl {
        pattern: String,
        #[serde(default = "ten")]
        timeout: f64,
    },
    /// Wait until an element (by text or CSS selector) is visible.
    WaitElement {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default = "ten")]
        timeout: f64,
    },
    WaitReady {
        #[serde(default = "ten")]
        timeout: f64,
    },
    Wait {
        ms: u64,
    },
    /// Evaluate JavaScript; the value is recorded in the report.
    Js {
        expression: String,
    },
    /// Evaluate JavaScript; its string form must contain `expect`
    /// (case-insensitive) or the step fails.
    Verify {
        expression: String,
        expect: String,
    },
}

fn yes() -> bool {
    true
}
fn five() -> f64 {
    5.0
}
fn ten() -> f64 {
    10.0
}

impl Step {
    /// Short label for reports, e.g. `click_text(Create account)`.
    pub fn label(&self) -> String {
        match self {
            Step::Goto { url, .. } => format!("goto({url})"),
            Step::ClickText { text, .. } => format!("click_text({text})"),
            Step::ClickXpath { xpath } => format!("click_xpath({xpath})"),
            Step::ClickHtmlId { html_id } => format!("click_html_id({html_id})"),
            Step::TypeText { name, text, .. } => format!("type_text({name}, {text})"),
            Step::PressKey { key } => format!("press_key({key})"),
            Step::WaitUrl { pattern, .. } => format!("wait_url({pattern})"),
            Step::WaitElement { text, selector, .. } => format!(
                "wait_element({})",
                text.as_deref().or(selector.as_deref()).unwrap_or("")
            ),
            Step::WaitReady { .. } => "wait_ready".into(),
            Step::Wait { ms } => format!("wait({ms}ms)"),
            Step::Js { expression } => format!("js({expression})"),
            Step::Verify { expression, .. } => format!("verify({expression})"),
        }
    }
}

/// What the runner did for one step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepReport {
    pub index: usize,
    pub step: String,
    /// The tool's JSON result (or the JS value for `js` / `verify`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    pub ms: u128,
}

/// Executes steps on one tab. Cloneable so graph tasks can share it.
#[derive(Clone)]
pub struct Runner {
    pub tab: Tab,
    /// Kept alive for the tab's lifetime.
    pub browser: Arc<BrowserClient>,
}

impl Runner {
    pub fn new(browser: BrowserClient, tab: Tab) -> Self {
        Self {
            tab,
            browser: Arc::new(browser),
        }
    }

    /// Connect like the CLI does: `port` (or the usual auto-detection) and
    /// the active tab (or the one whose URL contains `tab_url`).
    pub async fn connect(port: Option<u16>, tab_url: Option<&str>) -> Result<Self> {
        let mut browser = connect_browser(None, port).await?;
        let tab = get_active_tab(&mut browser, tab_url).await?;
        Ok(Self::new(browser, tab))
    }

    /// Run one step with `{{var}}` substitution. Tool-level soft failures
    /// (`clicked: false`, `matched: false`, `{"error": …}` …) are errors
    /// here so a graph can recover from them.
    pub async fn step(&self, step: &Step, vars: &HashMap<String, String>) -> Result<StepReport> {
        let started = std::time::Instant::now();
        let s = |t: &str| subst(t, vars);
        let tab = &self.tab;
        let result: Value = match step {
            Step::Goto { url, wait } => tools::page_goto(tab, &s(url), *wait).await?,
            Step::ClickText { text, timeout } => {
                tools::click_by_text(tab, &s(text), *timeout, false).await?
            }
            Step::ClickXpath { xpath } => tools::click_by_xpath(tab, &s(xpath)).await?,
            Step::ClickHtmlId { html_id } => tools::click_by_html_id(tab, &s(html_id)).await?,
            Step::TypeText {
                name,
                text,
                clear,
                enter,
                timeout,
            } => {
                tools::type_by_text(
                    tab,
                    &s(name),
                    &s(text),
                    TypeByTextOptions {
                        clear: *clear,
                        timeout: *timeout,
                        enter: *enter,
                        ..TypeByTextOptions::default()
                    },
                )
                .await?
            }
            Step::PressKey { key } => tools::press_key(tab, &s(key), None, 0).await?,
            Step::WaitUrl { pattern, timeout } => {
                tools::page_wait_url(tab, Some(&s(pattern)), None, *timeout).await?
            }
            Step::WaitElement {
                text,
                selector,
                timeout,
            } => {
                let t = text.as_deref().map(s);
                let sel = selector.as_deref().map(s);
                tools::page_wait_element(tab, t.as_deref(), sel.as_deref(), *timeout).await?
            }
            Step::WaitReady { timeout } => {
                let ready = tools::page_wait_ready(tab, *timeout, 0.0).await;
                serde_json::json!({ "ready": ready })
            }
            Step::Wait { ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(*ms)).await;
                Value::Null
            }
            Step::Js { expression } => js_value(tools::js_evaluate(tab, &s(expression)).await?),
            Step::Verify { expression, expect } => {
                let v = js_value(tools::js_evaluate(tab, &s(expression)).await?);
                let text = match &v {
                    Value::String(x) => x.clone(),
                    other => other.to_string(),
                };
                let want = s(expect);
                if !text.to_lowercase().contains(&want.to_lowercase()) {
                    return Err(Error::io(format!(
                        "verify failed: expected {want:?} in {text:?}"
                    )));
                }
                v
            }
        };
        if let Some(msg) = soft_failure(&result) {
            return Err(Error::io(format!("{}: {msg}", subst(&step.label(), vars))));
        }
        Ok(StepReport {
            index: 0,
            step: subst(&step.label(), vars),
            result: (!result.is_null()).then_some(result),
            ms: started.elapsed().as_millis(),
        })
    }
}

/// `js_evaluate` wraps the value as `{result, url_before, …}`; steps
/// record just the value.
fn js_value(mut v: Value) -> Value {
    match v.get_mut("result") {
        Some(r) => r.take(),
        None => v,
    }
}

/// adb's tools report "it did not work" as a successful JSON object rather
/// than an `Err`; detect those so a step can fail (and be retried).
pub fn soft_failure(v: &Value) -> Option<String> {
    let obj = v.as_object()?;
    if let Some(e) = obj.get("error") {
        return Some(e.as_str().map_or_else(|| e.to_string(), str::to_string));
    }
    for key in ["clicked", "typed", "matched", "found", "ready", "success"] {
        if obj.get(key).and_then(Value::as_bool) == Some(false) {
            let msg = obj
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("operation failed");
            return Some(format!("{key}=false: {msg}"));
        }
    }
    None
}

/// `{{name}}` → value.
pub fn subst(s: &str, vars: &HashMap<String, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn substitution_step_json_and_soft_failures() {
        let vars: HashMap<String, String> = [("who".to_string(), "Kai".to_string())].into();
        assert_eq!(subst("hi {{who}}!", &vars), "hi Kai!");
        let s: Vec<Step> = serde_json::from_str(
            r#"[{"goto":{"url":"x"}},{"click_text":{"text":"Go"}},{"type_text":{"name":"Q","text":"a","enter":true}},{"wait":{"ms":5}},{"verify":{"expression":"1","expect":"1"}}]"#,
        )
        .unwrap();
        assert_eq!(s.len(), 5);
        assert!(matches!(&s[0], Step::Goto { wait: true, .. }));
        assert!(matches!(&s[1], Step::ClickText { timeout, .. } if *timeout == 5.0));
        assert_eq!(s[2].label(), "type_text(Q, a)");

        assert!(soft_failure(&json!({"clicked": true})).is_none());
        assert!(soft_failure(&json!({"clicked": false, "message": "no"}))
            .unwrap()
            .contains("no"));
        assert_eq!(
            soft_failure(&json!({"error": "boom"})).as_deref(),
            Some("boom")
        );
        assert!(soft_failure(&json!("string")).is_none());
    }
}
