//! Execute a workflow: substitute, dispatch, bind, retry, report.

use crate::dispatch::Dispatch;
use crate::step::{Step, Workflow};
use crate::vars::Vars;
use serde::Serialize;
use serde_json::Value;
use sudohand_core::{Error, Result};

/// What happened for one step.
#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    pub id: String,
    /// The argv after `{{var}}` substitution.
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
    pub steps: Vec<StepReport>,
    /// Final variable bindings (for the caller / next stage).
    pub vars: std::collections::HashMap<String, Value>,
}

/// Runs workflows over a [`Dispatch`].
pub struct Runner<D: Dispatch> {
    dispatch: D,
}

impl<D: Dispatch> Runner<D> {
    pub fn new(dispatch: D) -> Self {
        Self { dispatch }
    }

    /// Check declared vars are supplied, then run every step in order.
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
        for step in &wf.steps {
            let rep = self.run_step(step, &mut vars);
            let ok = rep.ok;
            steps.push(rep);
            if !ok {
                return Ok(Report {
                    workflow: wf.name.clone(),
                    ok: false,
                    steps,
                    vars: vars.0,
                });
            }
        }
        Ok(Report {
            workflow: wf.name.clone(),
            ok: true,
            steps,
            vars: vars.0,
        })
    }

    fn run_step(&self, step: &Step, vars: &mut Vars) -> StepReport {
        // Substitute the argv up front so the report shows what actually ran.
        let argv: Result<Vec<String>> = step.run.iter().map(|a| vars.subst(a)).collect();
        let argv = match argv {
            Ok(a) => a,
            Err(e) => {
                return StepReport {
                    id: step.id.clone(),
                    run: step.run.clone(),
                    ok: step.optional,
                    attempts: 0,
                    result: None,
                    error: Some(e.to_string()),
                }
            }
        };

        let mut last_err = None;
        for attempt in 1..=step.attempts {
            match self.dispatch.call(&argv) {
                Ok(v) => {
                    if let Some(name) = &step.bind {
                        vars.set(name.clone(), v.clone());
                    }
                    return StepReport {
                        id: step.id.clone(),
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
            id: step.id.clone(),
            run: argv,
            ok: step.optional,
            attempts: step.attempts,
            result: None,
            error: last_err,
        }
    }
}
