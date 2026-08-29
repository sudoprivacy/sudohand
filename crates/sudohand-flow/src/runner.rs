//! Execute a workflow: substitute, dispatch actions / ask the user, bind,
//! retry, report.

use crate::dispatch::Dispatch;
use crate::prompt::{Prompter, Stdio};
use crate::step::{Action, Step, Workflow};
use crate::vars::Vars;
use serde::Serialize;
use serde_json::Value;
use sudohand_core::{Error, Result};

/// What happened for one step.
#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    pub id: String,
    /// The action argv after substitution (empty for interactive steps).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub run: Vec<String>,
    pub ok: bool,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The whole run.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub workflow: String,
    pub ok: bool,
    /// True when a Confirm step was declined (a clean cancel, not a failure).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
    pub steps: Vec<StepReport>,
    pub vars: std::collections::HashMap<String, Value>,
}

/// Runs workflows over a [`Dispatch`] (actions) and a [`Prompter`]
/// (interactive steps).
pub struct Runner<D: Dispatch, P: Prompter = Stdio> {
    dispatch: D,
    prompter: P,
}

impl<D: Dispatch> Runner<D, Stdio> {
    /// Actions via `dispatch`; prompts on stdin/stderr.
    pub fn new(dispatch: D) -> Self {
        Self {
            dispatch,
            prompter: Stdio,
        }
    }
}

impl<D: Dispatch, P: Prompter> Runner<D, P> {
    pub fn with_prompter(dispatch: D, prompter: P) -> Self {
        Self { dispatch, prompter }
    }

    /// Check declared vars, then run every step in order.
    pub fn run(&self, wf: &Workflow, mut vars: Vars) -> Result<Report> {
        let missing: Vec<&str> = wf
            .vars
            .iter()
            .filter(|v| vars.get(v).is_none())
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(Error::invalid(format!(
                "workflow {:?} needs --var {}",
                wf.name,
                missing
                    .iter()
                    .map(|m| format!("{m}=…"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )));
        }

        let mut steps = Vec::new();
        let mut cancelled = false;
        for step in &wf.steps {
            let (rep, cancel) = self.run_step(step, &mut vars);
            let ok = rep.ok;
            steps.push(rep);
            if cancel {
                cancelled = true;
                break;
            }
            if !ok {
                return Ok(Report {
                    workflow: wf.name.clone(),
                    ok: false,
                    cancelled: false,
                    steps,
                    vars: vars.0,
                });
            }
        }
        Ok(Report {
            workflow: wf.name.clone(),
            ok: !cancelled,
            cancelled,
            steps,
            vars: vars.0,
        })
    }

    /// Returns (report, cancelled).
    fn run_step(&self, step: &Step, vars: &mut Vars) -> (StepReport, bool) {
        match step {
            Step::Action(a) => (self.run_action(a, vars), false),
            Step::Confirm { id, message } => {
                let msg = match vars.subst(message) {
                    Ok(m) => m,
                    Err(e) => return (fail(id, e), false),
                };
                match self.prompter.confirm(&msg) {
                    Ok(true) => (ok0(id), false),
                    Ok(false) => (
                        StepReport {
                            id: id.clone(),
                            run: vec![],
                            ok: false,
                            attempts: 0,
                            result: None,
                            error: Some("cancelled".into()),
                        },
                        true,
                    ),
                    Err(e) => (fail(id, e), false),
                }
            }
            Step::Prompt {
                id,
                message,
                default,
                bind,
            } => {
                let msg = match vars.subst(message) {
                    Ok(m) => m,
                    Err(e) => return (fail(id, e), false),
                };
                let def = match default.as_ref().map(|d| vars.subst(d)).transpose() {
                    Ok(d) => d,
                    Err(e) => return (fail(id, e), false),
                };
                match self.prompter.line(&msg, def.as_deref()) {
                    Ok(v) => {
                        vars.set(bind.clone(), Value::String(v.clone()));
                        (
                            StepReport {
                                id: id.clone(),
                                run: vec![],
                                ok: true,
                                attempts: 1,
                                result: Some(Value::String(v)),
                                error: None,
                            },
                            false,
                        )
                    }
                    Err(e) => (fail(id, e), false),
                }
            }
            Step::Select {
                id,
                message,
                from,
                label,
                value,
                bind,
            } => (
                self.run_select(id, message, from, label, value, bind, vars),
                false,
            ),
        }
    }

    fn run_action(&self, a: &Action, vars: &mut Vars) -> StepReport {
        let argv: Result<Vec<String>> = a.run.iter().map(|s| vars.subst(s)).collect();
        let argv = match argv {
            Ok(v) => v,
            Err(e) => {
                let mut r = fail(&a.id, e);
                r.ok = a.optional;
                return r;
            }
        };
        let mut last_err = None;
        for attempt in 1..=a.attempts {
            match self.dispatch.call(&argv) {
                Ok(v) => {
                    if let Some(name) = &a.bind {
                        vars.set(name.clone(), v.clone());
                    }
                    return StepReport {
                        id: a.id.clone(),
                        run: argv,
                        ok: true,
                        attempts: attempt,
                        result: Some(v),
                        error: None,
                    };
                }
                Err(e) => last_err = Some(e.to_string()),
            }
        }
        StepReport {
            id: a.id.clone(),
            run: argv,
            ok: a.optional,
            attempts: a.attempts,
            result: None,
            error: last_err,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_select(
        &self,
        id: &str,
        message: &str,
        from: &str,
        label: &str,
        value: &str,
        bind: &str,
        vars: &mut Vars,
    ) -> StepReport {
        let items = match vars.lookup(from) {
            Some(Value::Array(a)) if !a.is_empty() => a,
            Some(Value::Array(_)) => return fail(id, Error::not_found(format!("{from} is empty"))),
            _ => {
                return fail(
                    id,
                    Error::invalid(format!("{from} is not an array variable")),
                )
            }
        };
        // Render each item's label with the item bound as `it`.
        let render = |tmpl: &str, item: &Value| -> Result<String> {
            let mut scope = vars.clone();
            scope.set("it", item.clone());
            scope.subst(tmpl)
        };
        let labels: Result<Vec<String>> = items.iter().map(|it| render(label, it)).collect();
        let labels = match labels {
            Ok(l) => l,
            Err(e) => return fail(id, e),
        };
        let idx = if items.len() == 1 {
            0
        } else {
            let msg = match vars.subst(message) {
                Ok(m) => m,
                Err(e) => return fail(id, e),
            };
            match self.prompter.select(&msg, &labels) {
                Ok(i) => i,
                Err(e) => return fail(id, e),
            }
        };
        let chosen = match render(value, &items[idx]) {
            Ok(v) => v,
            Err(e) => return fail(id, e),
        };
        vars.set(bind.to_string(), Value::String(chosen.clone()));
        StepReport {
            id: id.to_string(),
            run: vec![],
            ok: true,
            attempts: 1,
            result: Some(Value::String(chosen)),
            error: None,
        }
    }
}

fn ok0(id: &str) -> StepReport {
    StepReport {
        id: id.to_string(),
        run: vec![],
        ok: true,
        attempts: 1,
        result: None,
        error: None,
    }
}

fn fail(id: &str, e: Error) -> StepReport {
    StepReport {
        id: id.to_string(),
        run: vec![],
        ok: false,
        attempts: 0,
        result: None,
        error: Some(e.to_string()),
    }
}
