//! Steps, conditions and workflows: plain data, built in Rust.
//!
//! Execution is a graph, not just a line. Steps run in list order by
//! default, but any step can route elsewhere by id: an action can jump on
//! success (`on_ok`) or on failure (`on_fail`, instead of aborting), a
//! [`Step::Branch`] jumps on a [`Cond`], and [`Step::Goto`] jumps
//! unconditionally — so a workflow can *react* (retry a click until a VLM
//! `ask` says the screen changed, skip a confirm dialog that did not
//! appear, loop back on failure). A loop guard bounds total steps.
//!
//! Most steps are *actions* (a `suh` subcommand); a few are *interactive*
//! (ask the operator via the [`Prompter`](crate::Prompter)).

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
    /// Jump here after success (default: the next step in the list).
    pub on_ok: Option<String>,
    /// Jump here on failure instead of aborting (default: abort unless
    /// `optional`, which continues to the next step).
    pub on_fail: Option<String>,
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
    /// Failure continues to the next step instead of aborting.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
    /// Go to step `id` after this action succeeds.
    pub fn on_ok(mut self, id: &str) -> Self {
        self.on_ok = Some(id.into());
        self
    }
    /// Go to step `id` when this action fails (instead of aborting).
    pub fn on_fail(mut self, id: &str) -> Self {
        self.on_fail = Some(id.into());
        self
    }
}

/// A branch condition, evaluated against the current variables.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Cond {
    /// `var` exists and is not false / null / 0 / "" / "no".
    Truthy { var: String },
    /// `var` (looked up) equals `value` (a `{{…}}` template), case-insensitive.
    Eq { var: String, value: String },
    /// `var`'s string rendering contains `value` (a template), case-insensitive.
    Contains { var: String, value: String },
    /// Logical negation.
    Not { cond: Box<Cond> },
}

impl Cond {
    pub fn truthy(var: &str) -> Cond {
        Cond::Truthy { var: var.into() }
    }
    pub fn eq(var: &str, value: &str) -> Cond {
        Cond::Eq {
            var: var.into(),
            value: value.into(),
        }
    }
    pub fn contains(var: &str, value: &str) -> Cond {
        Cond::Contains {
            var: var.into(),
            value: value.into(),
        }
    }
    pub fn negate(self) -> Cond {
        Cond::Not {
            cond: Box::new(self),
        }
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
    /// Evaluate `cond`; jump to `then`/`else` (None = fall through).
    Branch {
        id: String,
        cond: Cond,
        then: Option<String>,
        els: Option<String>,
    },
    /// Jump unconditionally to `to`.
    Goto { id: String, to: String },
    /// Stop the workflow successfully.
    End { id: String },
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
            on_ok: None,
            on_fail: None,
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

    /// A branch on `cond`. Chain [`Branch::then`]/[`Branch::els`].
    pub fn branch(id: &str, cond: Cond) -> Branch {
        Branch {
            id: id.into(),
            cond,
            then: None,
            els: None,
        }
    }

    pub fn goto(id: &str, to: &str) -> Step {
        Step::Goto {
            id: id.into(),
            to: to.into(),
        }
    }

    pub fn end(id: &str) -> Step {
        Step::End { id: id.into() }
    }

    pub fn id(&self) -> &str {
        match self {
            Step::Action(a) => &a.id,
            Step::Confirm { id, .. }
            | Step::Prompt { id, .. }
            | Step::Select { id, .. }
            | Step::Branch { id, .. }
            | Step::Goto { id, .. }
            | Step::End { id } => id,
        }
    }
}

impl From<Action> for Step {
    fn from(a: Action) -> Self {
        Step::Action(a)
    }
}

/// Builder for a [`Step::Branch`].
pub struct Branch {
    id: String,
    cond: Cond,
    then: Option<String>,
    els: Option<String>,
}

impl Branch {
    /// Where to go when the condition holds.
    pub fn then(mut self, id: &str) -> Self {
        self.then = Some(id.into());
        self
    }
    /// Where to go when it does not.
    pub fn els(mut self, id: &str) -> Self {
        self.els = Some(id.into());
        self
    }
}

impl From<Branch> for Step {
    fn from(b: Branch) -> Self {
        Step::Branch {
            id: b.id,
            cond: b.cond,
            then: b.then,
            els: b.els,
        }
    }
}

/// A named, Rust-defined workflow. Steps run in list order unless one
/// routes elsewhere by id.
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
