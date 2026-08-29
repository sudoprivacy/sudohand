//! Vision-language model access for locating UI elements on a screenshot
//! and answering yes/no questions about it. The default implementation
//! talks to DashScope (Bailian) through its OpenAI-compatible endpoint with
//! `qwen3.6-27b` for grounding and `qwen3.6-flash` for cheap verification.
//!
//! Coordinates: Qwen-VL models answer in a **0–1000 normalized** space; the
//! [`Vlm`] trait keeps that convention and [`crate::workflow`] maps it onto
//! screenshot pixels and then screen points.

use serde_json::{json, Value};
use std::time::Duration;
use sudohand_core::{Error, Result};

/// A point in the model's 0–1000 normalized image space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormPoint {
    pub x: f64,
    pub y: f64,
}

pub trait Vlm: Send + Sync {
    /// Where on `png` is the element described by `description`?
    fn locate(&self, png: &[u8], description: &str) -> Result<NormPoint>;
    /// Free-form question about `png`; the runner uses it for yes/no checks.
    fn ask(&self, png: &[u8], question: &str) -> Result<String>;
}

/// DashScope / Bailian client (OpenAI-compatible `chat/completions`).
#[derive(Debug, Clone)]
pub struct DashScopeVlm {
    pub api_key: String,
    pub base_url: String,
    pub locate_model: String,
    pub ask_model: String,
    /// Ask the model to skip its reasoning phase (Qwen 3.6 thinks by default,
    /// which doubles latency without helping grounding).
    pub thinking: bool,
    pub timeout: Duration,
}

impl DashScopeVlm {
    pub const DEFAULT_LOCATE_MODEL: &'static str = "qwen3.6-27b";
    pub const DEFAULT_ASK_MODEL: &'static str = "qwen3.6-flash";

    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            locate_model: Self::DEFAULT_LOCATE_MODEL.into(),
            ask_model: Self::DEFAULT_ASK_MODEL.into(),
            thinking: false,
            timeout: Duration::from_secs(60),
        }
    }

    /// Key from `DASHSCOPE_API_KEY`, else the Bailian CLI's
    /// `~/.bailian/config.json`.
    pub fn from_env() -> Result<Self> {
        if let Ok(k) = std::env::var("DASHSCOPE_API_KEY") {
            if !k.trim().is_empty() {
                return Ok(Self::new(k.trim()));
            }
        }
        let home = std::env::var("HOME").map_err(|_| Error::internal("HOME is not set"))?;
        let path = std::path::Path::new(&home).join(".bailian/config.json");
        let cfg = std::fs::read_to_string(&path).map_err(|_| {
            Error::invalid("no DashScope key: set DASHSCOPE_API_KEY or run `bl auth login`")
        })?;
        let v: Value = serde_json::from_str(&cfg)
            .map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
        match v.get("api_key").and_then(Value::as_str) {
            Some(k) if !k.is_empty() => Ok(Self::new(k)),
            _ => Err(Error::invalid(format!("{}: no api_key", path.display()))),
        }
    }

    pub fn with_models(mut self, locate: &str, ask: &str) -> Self {
        self.locate_model = locate.into();
        self.ask_model = ask.into();
        self
    }

    fn chat(&self, model: &str, png: &[u8], prompt: &str) -> Result<String> {
        let data_url = format!("data:image/png;base64,{}", sudohand_core::b64::encode(png));
        let body = json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": data_url}},
                    {"type": "text", "text": prompt}
                ]
            }],
            "enable_thinking": self.thinking,
            "max_tokens": 1024,
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| Error::internal(format!("http client: {e}")))?;
        let resp = client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|e| Error::io(format!("vlm request ({model}): {e}")))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .map_err(|e| Error::io(format!("vlm response ({model}): {e}")))?;
        if !status.is_success() {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(Error::io(format!("vlm {model}: HTTP {status}: {msg}")));
        }
        v.pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::io(format!("vlm {model}: no content in response")))
    }
}

impl Vlm for DashScopeVlm {
    fn locate(&self, png: &[u8], description: &str) -> Result<NormPoint> {
        let prompt = format!(
            "这是一张 macOS 应用窗口截图。请定位下面描述的 UI 元素的中心点,\
             输出 JSON {{\"target\": [x, y]}},坐标为 0-1000 归一化,左上角为原点,\
             不要输出其他内容。\n目标:{description}"
        );
        let content = self.chat(&self.locate_model, png, &prompt)?;
        parse_point(&content).ok_or_else(|| {
            Error::io(format!(
                "vlm {}: could not parse a point from {content:?}",
                self.locate_model
            ))
        })
    }

    fn ask(&self, png: &[u8], question: &str) -> Result<String> {
        self.chat(&self.ask_model, png, question)
    }
}

/// Pull a point out of whatever shape the model chose:
/// `{"target":[x,y]}`, `[{"point_2d":[x,y],"label":..}]`,
/// `[{"bbox_2d":[x1,y1,x2,y2],..}]` (→ centre), or a bare `[x,y]`,
/// optionally wrapped in a ```json fence.
pub fn parse_point(content: &str) -> Option<NormPoint> {
    let mut s = content.trim();
    if let Some(start) = s.find('{').into_iter().chain(s.find('[')).min() {
        s = &s[start..];
    }
    if let Some(end) = s.rfind('}').into_iter().chain(s.rfind(']')).max() {
        s = &s[..=end];
    }
    let v: Value = serde_json::from_str(s).ok()?;
    if let Some(t) = v.get("target") {
        if let Some(p) = numeric_array(t) {
            return Some(p);
        }
    }
    first_point(&v)
}

fn numeric_array(v: &Value) -> Option<NormPoint> {
    let a = v.as_array()?;
    let nums: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
    if nums.len() != a.len() {
        return None;
    }
    match nums.as_slice() {
        [x, y] => Some(NormPoint { x: *x, y: *y }),
        [x1, y1, x2, y2] => Some(NormPoint {
            x: (x1 + x2) / 2.0,
            y: (y1 + y2) / 2.0,
        }),
        _ => None,
    }
}

fn first_point(v: &Value) -> Option<NormPoint> {
    if let Some(p) = numeric_array(v) {
        return Some(p);
    }
    match v {
        Value::Object(m) => {
            for key in ["point_2d", "point", "bbox_2d", "bbox", "center"] {
                if let Some(p) = m.get(key).and_then(numeric_array) {
                    return Some(p);
                }
            }
            m.values().find_map(first_point)
        }
        Value::Array(a) => a.iter().find_map(first_point),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_qwen_shape() {
        let p = |s: &str| parse_point(s).unwrap();
        assert_eq!(
            p(r#"{"target": [120, 30]}"#),
            NormPoint { x: 120.0, y: 30.0 }
        );
        assert_eq!(
            p("```json\n[\n\t{\"point_2d\": [109, 26], \"label\": \"search\"}\n]\n```"),
            NormPoint { x: 109.0, y: 26.0 }
        );
        assert_eq!(
            p(r#"[{"bbox_2d": [100, 10, 140, 30], "label": "x"}]"#),
            NormPoint { x: 120.0, y: 20.0 }
        );
        assert_eq!(p("[500, 720]"), NormPoint { x: 500.0, y: 720.0 });
        assert_eq!(
            p(r#"{"input": {"point_2d": [500, 720]}}"#),
            NormPoint { x: 500.0, y: 720.0 }
        );
        assert!(parse_point("I cannot see it").is_none());
        assert!(parse_point(r#"{"target": "left"}"#).is_none());
    }
}
