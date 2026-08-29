//! Variables shared across steps, and `{{name}}` / `{{name.field}}`
//! substitution into an argv.

use serde_json::Value;
use std::collections::HashMap;
use sudohand_core::{Error, Result};

/// The workflow's variable bindings (from `--var` and step `bind`s).
#[derive(Debug, Clone, Default)]
pub struct Vars(pub HashMap<String, Value>);

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        it: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Vars(
            it.into_iter()
                .map(|(k, v)| (k.into(), Value::String(v.into())))
                .collect(),
        )
    }

    pub fn set(&mut self, name: impl Into<String>, v: Value) {
        self.0.insert(name.into(), v);
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    /// Look up `a.b.c` — a top-level var then object fields / array indices.
    pub fn lookup(&self, path: &str) -> Option<Value> {
        let mut parts = path.split('.');
        let mut cur = self.0.get(parts.next()?)?.clone();
        for p in parts {
            cur = match &cur {
                Value::Object(m) => m.get(p)?.clone(),
                Value::Array(a) => a.get(p.parse::<usize>().ok()?)?.clone(),
                _ => return None,
            };
        }
        Some(cur)
    }

    /// Substitute `{{path}}` occurrences in one argument. A whole-string
    /// `{{x}}` yields x's scalar rendering; embedded ones interpolate.
    pub fn subst(&self, arg: &str) -> Result<String> {
        if !arg.contains("{{") {
            return Ok(arg.to_string());
        }
        let mut out = String::new();
        let mut rest = arg;
        while let Some(open) = rest.find("{{") {
            out.push_str(&rest[..open]);
            let after = &rest[open + 2..];
            let close = after
                .find("}}")
                .ok_or_else(|| Error::invalid(format!("unclosed {{{{ in {arg:?}")))?;
            let path = after[..close].trim();
            let val = self
                .lookup(path)
                .ok_or_else(|| Error::invalid(format!("undefined variable {{{{{path}}}}}")))?;
            out.push_str(&render(&val));
            rest = &after[close + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// Scalars render bare; objects/arrays as compact JSON.
fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}
