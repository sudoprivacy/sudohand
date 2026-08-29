//! Name → workflow registry. Workflows are graphs written in code; a
//! [`Registry`] maps a name to a builder (`Fn(&Runner) -> Graph`) plus its
//! description and required variables. This crate ships no app-specific
//! graphs — [`Registry::builtins`] is empty; extensions and integrators
//! register theirs with [`Registry::register_fn`] and resolve/validate/run
//! them by name.

use crate::flow::{run_graph, FlowGraph as Graph};
use crate::workflow::{Runner, StepReport};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use sudohand_core::{Error, Result};

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
    /// The crate's own workflows: none (app knowledge lives in
    /// extensions). Kept as the conventional starting point for
    /// [`Registry::register_fn`].
    pub fn builtins() -> Self {
        Self::new()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow;
    use crate::workflow::Step;
    use graph_flow::Task;

    fn err_of<T>(r: Result<T>) -> Error {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    struct NoVlm;
    impl crate::vlm::Vlm for NoVlm {
        fn locate(&self, _: &[u8], _: &str) -> Result<crate::vlm::NormPoint> {
            unreachable!()
        }
        fn ask(&self, _: &[u8], _: &str) -> Result<String> {
            unreachable!()
        }
    }

    fn runner() -> Runner {
        Runner::new(
            crate::fake::FakeBackend::with_running(&["com.example.app"]),
            Arc::new(NoVlm),
        )
    }

    fn hello(runner: &Runner) -> Result<Graph> {
        let t = flow::StepTask::new(
            "say",
            runner,
            "com.example.app",
            Step::Type {
                text: "hi {{who}}".into(),
            },
        );
        let done = Arc::new(flow::EndTask);
        flow::graph_flow::GraphBuilder::new("hello")
            .add_task(t.clone())
            .add_task(done.clone())
            .set_start_task(t.id())
            .add_edge(t.id(), done.id())
            .build()
            .map_err(|e| Error::internal(e.to_string()))
    }

    #[test]
    fn register_list_override_and_var_check() {
        let mut r = Registry::builtins();
        assert!(r.list().is_empty());
        r.register_fn("hello", "says hi", &["who"], "test", hello);
        r.register_fn("wechat-send", "first", &[], "test0", hello);
        // same name replaces the earlier registration
        r.register_fn("wechat-send", "override", &[], "test", hello);
        let names: Vec<_> = r.list().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["hello", "wechat-send"]);
        let w = r.get("wechat-send").unwrap();
        assert_eq!((w.description(), w.source().as_str()), ("override", "test"));

        let rn = runner();
        let err = err_of(r.prepare("hello", &rn, &HashMap::new()));
        assert!(matches!(err, Error::InvalidInput(_)), "{err}");
        assert!(err.to_string().contains("--var who="));
        assert!(matches!(
            err_of(r.prepare("nope", &rn, &HashMap::new())),
            Error::NotFound(_)
        ));
        let vars: HashMap<String, String> = [("who".to_string(), "x".to_string())].into();
        assert!(r.prepare("hello", &rn, &vars).is_ok());
    }

    #[tokio::test]
    async fn run_by_name() {
        let mut r = Registry::new();
        r.register_fn("hello", "", &["who"], "test", hello);
        let vars: HashMap<String, String> = [("who".to_string(), "Kai".to_string())].into();
        let report = r.run("hello", &runner(), vars).await.unwrap();
        assert_eq!(report.len(), 1);
        assert!(report[0].step.starts_with("say: type(hi {{who}})"));
    }
}
