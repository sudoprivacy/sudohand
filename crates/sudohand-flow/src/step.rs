//! Steps and workflows: plain data, built in Rust. Most steps run an
//! *action* (a `suh` subcommand); a few are *interactive* — they ask the
//! person running the workflow (account picker, confirmation) via the
//! [`Prompter`](crate::Prompter), so an end-to-end flow like `wx init` is one
//! workflow instead of hand-written command glue.

use serde::Serialize;

/// One action invocation: a `suh` argv without the leading `suh`
/// (`["desktop","locate","--find","{{q}}"]`), args carrying `{{var}}`
/// templates resolved at run time. Its JSON result may `bind` into a var.
#[derive(Debug, Clone, Serialize)]
pub struct Action {
    pub id: String,
    pub run: Vec<String>,
    pub bind: Option<String>,
    pub attempts: u32,
    pub optional: bool,
}

impl Action {
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

/// One workflow step.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    /// Run a `suh` action.
    Action(Action),
    /// Ask a yes/no question; "no" stops the workflow (a clean cancel).
    Confirm { id: String, message: String },
    /// Read a line into `bind` (message and default may use `{{var}}`).
    Prompt {
        id: String,
        message: String,
        default: Option<String>,
        bind: String,
    },
    /// Choose one element of the array at `from`. `label`/`value` are
    /// templates evaluated with the chosen element bound as `it`; `value`
    /// is stored in `bind`. A single-element array is chosen automatically.
    Select {
        id: String,
        message: String,
        from: String,
        label: String,
        value: String,
        bind: String,
    },
}

impl Step {
    /// An action step named `id` running `argv` (domain, action, args…).
    pub fn run<S: Into<String>>(id: &str, argv: impl IntoIterator<Item = S>) -> Action {
        Action {
            id: id.into(),
            run: argv.into_iter().map(Into::into).collect(),
            bind: None,
            attempts: 1,
            optional: false,
        }
    }

    pub fn confirm(id: &str, message: &str) -> Step {
        Step::Confirm {
            id: id.into(),
            message: message.into(),
        }
    }

    pub fn prompt(id: &str, message: &str, bind: &str) -> Step {
        Step::Prompt {
            id: id.into(),
            message: message.into(),
            default: None,
            bind: bind.into(),
        }
    }

    pub fn prompt_default(id: &str, message: &str, default: &str, bind: &str) -> Step {
        Step::Prompt {
            id: id.into(),
            message: message.into(),
            default: Some(default.into()),
            bind: bind.into(),
        }
    }

    pub fn select(
        id: &str,
        message: &str,
        from: &str,
        label: &str,
        value: &str,
        bind: &str,
    ) -> Step {
        Step::Select {
            id: id.into(),
            message: message.into(),
            from: from.into(),
            label: label.into(),
            value: value.into(),
            bind: bind.into(),
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Step::Action(a) => &a.id,
            Step::Confirm { id, .. } | Step::Prompt { id, .. } | Step::Select { id, .. } => id,
        }
    }
}

impl From<Action> for Step {
    fn from(a: Action) -> Self {
        Step::Action(a)
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
    pub fn var(mut self, name: &str) -> Self {
        self.vars.push(name.into());
        self
    }
    pub fn step(mut self, s: impl Into<Step>) -> Self {
        self.steps.push(s.into());
        self
    }
}
