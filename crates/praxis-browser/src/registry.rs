//! Name → workflow registry. Workflows are graphs written in code; a
//! [`Registry`] maps a name to a builder (`Fn(&Runner) -> Graph`) plus its
//! description and required variables. [`Registry::builtins`] holds the
//! crate's own (`wechat-send`); an integrator registers its graphs with
//! [`Registry::register_fn`] and resolves/validates/runs them by name.
//! Built-ins: `form-signup` and `page-extract` (reference graphs).

// The ported adb modules are clippy-pedantic; the flow layer follows the
// workspace's standard clippy level like praxis-desktop's.
#![allow(clippy::pedantic)]

use crate::flow::{self, run_graph, FlowGraph as Graph};
use crate::workflow::{Runner, StepReport};
use praxis_core::{Error, Result};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Something that can produce a graph for a [`Runner`].
pub trait WorkflowDef: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// Variables the workflow requires (`{{name}}`).
    fn vars(&self) -> &[String];
    /// Who registered it: `"builtin"` for the crate's own, else whatever
    /// the integrator passed.
    fn source(&self) -> String;
    fn graph(&self, runner: &Runner) -> Result<Graph>;
}

/// A workflow defined in Rust by a closure.
pub struct FnWorkflow {
    pub name: String,
    pub description: String,
    pub vars: Vec<String>,
    pub source: String,
    #[allow(clippy::type_complexity)]
    pub build: Box<dyn Fn(&Runner) -> Result<Graph> + Send + Sync>,
}

impl WorkflowDef for FnWorkflow {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn vars(&self) -> &[String] {
        &self.vars
    }
    fn source(&self) -> String {
        self.source.clone()
    }
    fn graph(&self, runner: &Runner) -> Result<Graph> {
        (self.build)(runner)
    }
}

/// One row of [`Registry::list`].
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub description: String,
    pub vars: Vec<String>,
    pub source: String,
}

#[derive(Default)]
pub struct Registry {
    defs: BTreeMap<String, Arc<dyn WorkflowDef>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Built-ins only.
    pub fn builtins() -> Self {
        let mut r = Self::new();
        r.register_fn(
            "form-signup",
            "Open a page, fill Username/Email, submit `Create account`, verify the result page (reference graph with retry)",
            &["url", "username", "email"],
            "builtin",
            flow::form_signup,
        );
        r.register_fn(
            "page-extract",
            "Open a URL, wait until idle, evaluate a JS expression and return its value",
            &["url", "expression"],
            "builtin",
            flow::page_extract,
        );
        r
    }

    pub fn register(&mut self, def: Arc<dyn WorkflowDef>) {
        self.defs.insert(def.name().to_string(), def);
    }

    /// Register a graph builder under `name`, tagged `source` (`"builtin"`
    /// for the crate's own; integrators use their crate name).
    pub fn register_fn(
        &mut self,
        name: &str,
        description: &str,
        vars: &[&str],
        source: &str,
        build: impl Fn(&Runner) -> Result<Graph> + Send + Sync + 'static,
    ) {
        self.register(Arc::new(FnWorkflow {
            name: name.into(),
            description: description.into(),
            vars: vars.iter().map(|s| s.to_string()).collect(),
            source: source.into(),
            build: Box::new(build),
        }));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn WorkflowDef>> {
        self.defs.get(name).cloned()
    }

    pub fn list(&self) -> Vec<Entry> {
        self.defs
            .values()
            .map(|d| Entry {
                name: d.name().to_string(),
                description: d.description().to_string(),
                vars: d.vars().to_vec(),
                source: d.source(),
            })
            .collect()
    }

    /// Resolve `name` and check that every declared variable was supplied.
    pub fn prepare(
        &self,
        name: &str,
        runner: &Runner,
        vars: &HashMap<String, String>,
    ) -> Result<Graph> {
        let def = self.get(name).ok_or_else(|| {
            let known: Vec<_> = self.defs.keys().cloned().collect();
            Error::not_found(format!(
                "unknown workflow {name:?}; known: {}",
                known.join(", ")
            ))
        })?;
        let missing: Vec<_> = def
            .vars()
            .iter()
            .filter(|v| !vars.contains_key(*v))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(Error::invalid(format!(
                "workflow {name:?} needs --var {}",
                missing
                    .iter()
                    .map(|m| format!("{m}=…"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )));
        }
        def.graph(runner)
    }

    /// `prepare` + run to completion.
    pub async fn run(
        &self,
        name: &str,
        runner: &Runner,
        vars: HashMap<String, String>,
    ) -> Result<Vec<StepReport>> {
        let graph = self.prepare(name, runner, &vars)?;
        run_graph(graph, vars).await
    }
}
