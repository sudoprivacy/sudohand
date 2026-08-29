//! Execute a workflow by compiling it to a `graph_flow` graph and running
//! that — so branching, `GoTo` loops and the step-count guard come from the
//! same orchestration library the desktop flows use. Each declarative
//! [`Step`] becomes one graph task; a task runs its action through the
//! [`Dispatch`] (or asks the [`Prompter`]) inside `spawn_blocking`, since
//! those calls are synchronous, and returns the `graph_flow` `NextAction`
//! that routes the graph.

use crate::dispatch::Dispatch;
use crate::prompt::{Prompter, Stdio};
use crate::step::{Cond, Step, Workflow};
use crate::vars::Vars;
use async_trait::async_trait;
use graph_flow::{
    Context, ExecutionStatus, FlowRunner, GraphBuilder, InMemorySessionStorage, NextAction,
    Session, SessionStorage, Task, TaskResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use sudohand_core::{Error, Result};

const VARS: &str = "__vars";
const REPORT: &str = "__report";
const OUTCOME: &str = "__outcome"; // "fail" | "cancel" (absent = ok)

/// What happened for one executed step (a step id can repeat when the graph
/// loops).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run: Vec<String>,
    pub ok: bool,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The whole run.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub workflow: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
    pub steps: Vec<StepReport>,
    pub vars: HashMap<String, Value>,
}

/// Default cap on total executed steps (loops included).
pub const DEFAULT_MAX_STEPS: usize = 200;

/// Runs workflows over a [`Dispatch`] (actions) and a [`Prompter`]
/// (interactive steps).
pub struct Runner {
    dispatch: Arc<dyn Dispatch>,
    prompter: Arc<dyn Prompter>,
    max_steps: usize,
}

impl Runner {
    /// Actions via `dispatch`; prompts on stdin/stderr.
    pub fn new(dispatch: impl Dispatch + 'static) -> Self {
        Self {
            dispatch: Arc::new(dispatch),
            prompter: Arc::new(Stdio),
            max_steps: DEFAULT_MAX_STEPS,
        }
    }

    pub fn with_prompter(
        dispatch: impl Dispatch + 'static,
        prompter: impl Prompter + 'static,
    ) -> Self {
        Self {
            dispatch: Arc::new(dispatch),
            prompter: Arc::new(prompter),
            max_steps: DEFAULT_MAX_STEPS,
        }
    }

    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }

    /// Check declared vars, compile to a graph, and run it to completion.
    pub fn run(&self, wf: &Workflow, vars: Vars) -> Result<Report> {
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
        if wf.steps.is_empty() {
            return Ok(Report {
                workflow: wf.name.clone(),
                ok: true,
                cancelled: false,
                steps: vec![],
                vars: vars.0,
            });
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::internal(format!("tokio: {e}")))?;
        rt.block_on(self.run_async(wf, vars))
    }

    async fn run_async(&self, wf: &Workflow, vars: Vars) -> Result<Report> {
        // Sequential successor id per step (None = end after it).
        let ids: Vec<String> = wf.steps.iter().map(|s| s.id().to_string()).collect();
        let mut builder = GraphBuilder::new(wf.name.clone());
        for (i, step) in wf.steps.iter().enumerate() {
            let next = ids.get(i + 1).cloned();
            builder = builder.add_task(Arc::new(StepTask {
                step: step.clone(),
                next,
                dispatch: self.dispatch.clone(),
                prompter: self.prompter.clone(),
            }));
        }
        let graph = builder
            .set_start_task(ids[0].clone())
            .with_max_execution_steps(self.max_steps)
            .build()
            .map_err(|e| Error::internal(format!("graph: {e}")))?;

        let storage = Arc::new(InMemorySessionStorage::new());
        let runner = FlowRunner::new(Arc::new(graph), storage.clone());
        let sid = format!(
            "flow-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let session = Session::new_from_task(sid.clone(), &ids[0]);
        session
            .context
            .set(VARS, &vars.0)
            .map_err(|e| Error::internal(format!("context: {e}")))?;
        storage
            .save(session)
            .await
            .map_err(|e| Error::internal(format!("session: {e}")))?;

        // FlowRunner runs one task per call; GoTo returns Paused without
        // counting toward graph-flow's own guard, so bound the drive loop
        // here (a GoTo cycle would otherwise spin forever).
        let mut budget = self.max_steps;
        loop {
            match runner.run(&sid).await {
                Ok(r) => match r.status {
                    ExecutionStatus::Completed => break,
                    ExecutionStatus::WaitingForInput => break,
                    ExecutionStatus::Paused { .. } => {
                        budget = budget.saturating_sub(1);
                        if budget == 0 {
                            if let Some(s) = storage.get(&sid).await.ok().flatten() {
                                push_report(
                                    &s.context,
                                    StepReport {
                                        id: "<loop-guard>".into(),
                                        run: vec![],
                                        ok: false,
                                        attempts: 0,
                                        result: None,
                                        error: Some(format!(
                                            "exceeded {} steps (cycle?)",
                                            self.max_steps
                                        )),
                                    },
                                );
                                mark(&s.context, "fail");
                                let _ = storage.save(s).await;
                            }
                            break;
                        }
                        continue;
                    }
                },
                Err(e) => return Err(Error::io(format!("flow error: {e}"))),
            }
        }

        let ctx = storage
            .get(&sid)
            .await
            .ok()
            .flatten()
            .map(|s| s.context)
            .ok_or_else(|| Error::internal("session vanished"))?;
        let steps: Vec<StepReport> = ctx.get(REPORT).unwrap_or_default();
        let out_vars: HashMap<String, Value> = ctx.get(VARS).unwrap_or_default();
        let outcome: Option<String> = ctx.get(OUTCOME);
        let cancelled = outcome.as_deref() == Some("cancel");
        let failed = outcome.as_deref() == Some("fail");
        Ok(Report {
            workflow: wf.name.clone(),
            ok: !cancelled && !failed,
            cancelled,
            steps,
            vars: out_vars,
        })
    }
}

/// One graph task = one declarative [`Step`].
struct StepTask {
    step: Step,
    next: Option<String>,
    dispatch: Arc<dyn Dispatch>,
    prompter: Arc<dyn Prompter>,
}

impl StepTask {
    /// Follow the sequential edge, or end if this was the last step.
    fn seq(&self) -> NextAction {
        match &self.next {
            Some(id) => NextAction::GoTo(id.clone()),
            None => NextAction::End,
        }
    }
}

fn load_vars(ctx: &Context) -> Vars {
    Vars(ctx.get::<HashMap<String, Value>>(VARS).unwrap_or_default())
}

fn save_vars(ctx: &Context, vars: &Vars) {
    let _ = ctx.set(VARS, &vars.0);
}

fn push_report(ctx: &Context, r: StepReport) {
    let mut all: Vec<StepReport> = ctx.get(REPORT).unwrap_or_default();
    all.push(r);
    let _ = ctx.set(REPORT, all);
}

fn mark(ctx: &Context, outcome: &str) {
    let _ = ctx.set(OUTCOME, outcome.to_string());
}

#[async_trait]
impl Task for StepTask {
    fn id(&self) -> &str {
        self.step.id()
    }

    async fn run(&self, ctx: Context) -> graph_flow::Result<TaskResult> {
        let mut vars = load_vars(&ctx);
        let next = match &self.step {
            Step::Action(a) => {
                // Substitute argv, then run the (blocking) dispatch off-thread.
                let argv: std::result::Result<Vec<String>, Error> =
                    a.run.iter().map(|s| vars.subst(s)).collect();
                match argv {
                    Err(e) => {
                        push_report(&ctx, fail(&a.id, e));
                        mark(&ctx, "fail");
                        NextAction::End
                    }
                    Ok(argv) => {
                        let d = self.dispatch.clone();
                        let mut last_err = None;
                        let mut done = None;
                        for attempt in 1..=a.attempts {
                            let d2 = d.clone();
                            let argv2 = argv.clone();
                            let res = tokio::task::spawn_blocking(move || d2.call(&argv2))
                                .await
                                .map_err(|e| {
                                    graph_flow::GraphError::TaskExecutionFailed(e.to_string())
                                })?;
                            match res {
                                Ok(v) => {
                                    done = Some((attempt, v));
                                    break;
                                }
                                Err(e) => last_err = Some(e.to_string()),
                            }
                        }
                        match done {
                            Some((attempt, v)) => {
                                if let Some(name) = &a.bind {
                                    vars.set(name.clone(), v.clone());
                                    save_vars(&ctx, &vars);
                                }
                                push_report(
                                    &ctx,
                                    StepReport {
                                        id: a.id.clone(),
                                        run: argv,
                                        ok: true,
                                        attempts: attempt,
                                        result: Some(v),
                                        error: None,
                                    },
                                );
                                match &a.on_ok {
                                    Some(id) => NextAction::GoTo(id.clone()),
                                    None => self.seq(),
                                }
                            }
                            None => {
                                push_report(
                                    &ctx,
                                    StepReport {
                                        id: a.id.clone(),
                                        run: argv,
                                        ok: false,
                                        attempts: a.attempts,
                                        result: None,
                                        error: last_err,
                                    },
                                );
                                if let Some(id) = &a.on_fail {
                                    NextAction::GoTo(id.clone())
                                } else if a.optional {
                                    self.seq()
                                } else {
                                    mark(&ctx, "fail");
                                    NextAction::End
                                }
                            }
                        }
                    }
                }
            }
            Step::Confirm { id, message } => {
                let msg = vars.subst(message).unwrap_or_else(|_| message.clone());
                let p = self.prompter.clone();
                let ok = tokio::task::spawn_blocking(move || p.confirm(&msg))
                    .await
                    .map_err(|e| graph_flow::GraphError::TaskExecutionFailed(e.to_string()))?;
                match ok {
                    Ok(true) => {
                        push_report(&ctx, ok0(id));
                        self.seq()
                    }
                    Ok(false) => {
                        push_report(
                            &ctx,
                            StepReport {
                                id: id.clone(),
                                run: vec![],
                                ok: false,
                                attempts: 0,
                                result: None,
                                error: Some("cancelled".into()),
                            },
                        );
                        mark(&ctx, "cancel");
                        NextAction::End
                    }
                    Err(e) => {
                        push_report(&ctx, fail(id, e));
                        mark(&ctx, "fail");
                        NextAction::End
                    }
                }
            }
            Step::Prompt {
                id,
                message,
                default,
                bind,
            } => {
                let msg = vars.subst(message).unwrap_or_else(|_| message.clone());
                let def = default
                    .as_ref()
                    .map(|d| vars.subst(d).unwrap_or_else(|_| d.clone()));
                let p = self.prompter.clone();
                let line = tokio::task::spawn_blocking(move || p.line(&msg, def.as_deref()))
                    .await
                    .map_err(|e| graph_flow::GraphError::TaskExecutionFailed(e.to_string()))?;
                match line {
                    Ok(v) => {
                        vars.set(bind.clone(), Value::String(v.clone()));
                        save_vars(&ctx, &vars);
                        push_report(
                            &ctx,
                            StepReport {
                                id: id.clone(),
                                run: vec![],
                                ok: true,
                                attempts: 1,
                                result: Some(Value::String(v)),
                                error: None,
                            },
                        );
                        self.seq()
                    }
                    Err(e) => {
                        push_report(&ctx, fail(id, e));
                        mark(&ctx, "fail");
                        NextAction::End
                    }
                }
            }
            Step::Select {
                id,
                message,
                from,
                label,
                value,
                bind,
            } => {
                match self
                    .run_select(id, message, from, label, value, bind, &mut vars)
                    .await
                {
                    Ok(rep) => {
                        save_vars(&ctx, &vars);
                        push_report(&ctx, rep);
                        self.seq()
                    }
                    Err(e) => {
                        push_report(&ctx, fail(id, e));
                        mark(&ctx, "fail");
                        NextAction::End
                    }
                }
            }
            Step::Branch {
                id: _,
                cond,
                then,
                els,
            } => {
                let hit = eval(cond, &vars);
                match if hit { then } else { els } {
                    Some(to) => NextAction::GoTo(to.clone()),
                    None => self.seq(),
                }
            }
            Step::Goto { id: _, to } => NextAction::GoTo(to.clone()),
            Step::End { id: _ } => NextAction::End,
        };
        Ok(TaskResult::new(None, next))
    }
}

impl StepTask {
    #[allow(clippy::too_many_arguments)]
    async fn run_select(
        &self,
        _id: &str,
        message: &str,
        from: &str,
        label: &str,
        value: &str,
        bind: &str,
        vars: &mut Vars,
    ) -> Result<StepReport> {
        let items = match vars.lookup(from) {
            Some(Value::Array(a)) if !a.is_empty() => a,
            Some(Value::Array(_)) => return Err(Error::not_found(format!("{from} is empty"))),
            _ => return Err(Error::invalid(format!("{from} is not an array variable"))),
        };
        let render = |tmpl: &str, item: &Value| -> Result<String> {
            let mut scope = vars.clone();
            scope.set("it", item.clone());
            scope.subst(tmpl)
        };
        let labels: Vec<String> = items
            .iter()
            .map(|it| render(label, it))
            .collect::<Result<_>>()?;
        let idx = if items.len() == 1 {
            0
        } else {
            let msg = vars.subst(message)?;
            let p = self.prompter.clone();
            let labels2 = labels.clone();
            tokio::task::spawn_blocking(move || p.select(&msg, &labels2))
                .await
                .map_err(|e| Error::internal(e.to_string()))??
        };
        let chosen = render(value, &items[idx])?;
        vars.set(bind.to_string(), Value::String(chosen.clone()));
        Ok(StepReport {
            id: _id.to_string(),
            run: vec![],
            ok: true,
            attempts: 1,
            result: Some(Value::String(chosen)),
            error: None,
        })
    }
}

fn truthy(v: Option<Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => {
            let s = s.trim().to_ascii_lowercase();
            !(s.is_empty() || s == "no" || s == "false" || s == "0")
        }
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn render_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn eval(cond: &Cond, vars: &Vars) -> bool {
    match cond {
        Cond::Truthy { var } => truthy(vars.lookup(var)),
        Cond::Eq { var, value } => {
            let lhs = vars
                .lookup(var)
                .map(|v| render_scalar(&v))
                .unwrap_or_default();
            let rhs = vars.subst(value).unwrap_or_default();
            lhs.eq_ignore_ascii_case(&rhs)
        }
        Cond::Contains { var, value } => {
            let lhs = vars
                .lookup(var)
                .map(|v| render_scalar(&v))
                .unwrap_or_default();
            let rhs = vars.subst(value).unwrap_or_default();
            lhs.to_ascii_lowercase().contains(&rhs.to_ascii_lowercase())
        }
        Cond::Not { cond } => !eval(cond, vars),
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
