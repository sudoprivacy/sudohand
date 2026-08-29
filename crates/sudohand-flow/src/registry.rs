//! Name → workflow registry. Workflows are built by Rust closures so they
//! can be parameterised; an extension registers its own and looks them up by
//! name.

use crate::step::Workflow;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use sudohand_core::{Error, Result};

pub type WorkflowFn = Arc<dyn Fn() -> Workflow + Send + Sync>;

/// A registry listing entry.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub description: String,
    pub vars: Vec<String>,
    pub source: String,
}

#[derive(Default, Clone)]
pub struct Registry {
    defs: BTreeMap<String, (WorkflowFn, String)>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `build` under `name`, tagged `source` (the extension name).
    pub fn register(&mut self, name: &str, source: &str, build: WorkflowFn) {
        self.defs
            .insert(name.to_string(), (build, source.to_string()));
    }

    pub fn get(&self, name: &str) -> Option<Workflow> {
        self.defs.get(name).map(|(b, _)| b())
    }

    pub fn list(&self) -> Vec<Entry> {
        self.defs
            .iter()
            .map(|(name, (build, source))| {
                let wf = build();
                Entry {
                    name: name.clone(),
                    description: wf.description,
                    vars: wf.vars,
                    source: source.clone(),
                }
            })
            .collect()
    }

    /// Resolve `name` or a not-found error listing the known names.
    pub fn resolve(&self, name: &str) -> Result<Workflow> {
        self.get(name).ok_or_else(|| {
            Error::not_found(format!(
                "unknown workflow {name:?}; known: {}",
                self.defs.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        })
    }
}
