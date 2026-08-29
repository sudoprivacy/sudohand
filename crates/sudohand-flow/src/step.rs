//! A step and a workflow: plain data, built in Rust.

use serde::Serialize;

/// One action invocation. `run` is a `suh` argv without the leading `suh`
/// (`["desktop","locate","--find","{{q}}"]`); its args may carry `{{var}}`
/// templates resolved at run time.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub id: String,
    /// domain + action + args, e.g. `["fs","read","--path","{{p}}"]`.
    pub run: Vec<String>,
    /// Bind the result JSON into this variable for later `{{…}}`.
    pub bind: Option<String>,
    /// Retry this many times on failure before the workflow fails.
    pub attempts: u32,
    /// If set, the step may fail without failing the workflow; its error is
    /// recorded and execution continues.
    pub optional: bool,
}

impl Step {
    /// A step named `id` running `argv` (domain, action, args…).
    pub fn run<S: Into<String>>(id: &str, argv: impl IntoIterator<Item = S>) -> Self {
        Step {
            id: id.into(),
            run: argv.into_iter().map(Into::into).collect(),
            bind: None,
            attempts: 1,
            optional: false,
        }
    }

    /// Bind the result JSON to `var`.
    pub fn bind(mut self, var: &str) -> Self {
        self.bind = Some(var.into());
        self
    }

    pub fn attempts(mut self, n: u32) -> Self {
        self.attempts = n.max(1);
        self
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

/// A named, Rust-defined sequence of steps with declared variables.
#[derive(Debug, Clone, Serialize)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub vars: Vec<String>,
    pub steps: Vec<Step>,
}

impl Workflow {
    pub fn new(name: &str) -> Self {
        Workflow {
            name: name.into(),
            description: String::new(),
            vars: Vec::new(),
            steps: Vec::new(),
        }
    }

    pub fn describe(mut self, d: &str) -> Self {
        self.description = d.into();
        self
    }

    /// Declare a required variable (checked before the workflow runs).
    pub fn var(mut self, name: &str) -> Self {
        self.vars.push(name.into());
        self
    }

    pub fn step(mut self, s: Step) -> Self {
        self.steps.push(s);
        self
    }
}
