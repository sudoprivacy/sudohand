//! LangGraph-style orchestration (via [`graph_flow`]) over the workflow
//! step primitives. Feature `flow`.
//!
//! Nodes are [`StepTask`] (run one [`Step`]; on failure jump to a recovery
//! node up to `max_attempts` times, else abort), [`AskTask`] (screenshot +
//! yes/no question → a boolean in the context, used by conditional edges)
//! and [`EndTask`]. Shared state lives in the graph-flow [`Context`]:
//! `vars` (the `{{name}}` substitutions), `report` (every [`StepReport`] so
//! far) and per-task attempt counters.
//!
//! A reference graph (WeChat "send a message": conditional edge, GoTo
//! recovery loop, abort on exhausted attempts) lives in
//! `sudoprivacy/suh-wx` (`src/flows.rs`) together with its tests; this crate
//! carries no app-specific knowledge.

use crate::workflow::{Runner, Step, StepReport, WindowPick};
use async_trait::async_trait;
use graph_flow::{
    Context, ExecutionStatus, FlowRunner, Graph, GraphError, InMemorySessionStorage, NextAction,
    Session, SessionStorage, Task, TaskResult,
};
use std::collections::HashMap;
use std::sync::Arc;
use sudohand_core::{Error, Result};

pub use graph_flow;
pub use graph_flow::Graph as FlowGraph;

const VARS: &str = "vars";
const REPORT: &str = "report";

fn vars(ctx: &Context) -> HashMap<String, String> {
    ctx.get(VARS).unwrap_or_default()
}

fn push_report(ctx: &Context, task: &str, reps: Vec<StepReport>) -> graph_flow::Result<()> {
    let mut all: Vec<StepReport> = ctx.get(REPORT).unwrap_or_default();
    for mut r in reps {
        r.index = all.len();
        r.step = format!("{task}: {}", r.step);
        all.push(r);
    }
    ctx.set(REPORT, all)
}

fn fail(msg: impl Into<String>) -> GraphError {
    GraphError::TaskExecutionFailed(msg.into())
}

/// Run one workflow step. On error, jump to `on_fail` while attempts remain;
/// otherwise the whole flow fails with the step's error.
pub struct StepTask {
    pub id: String,
    pub runner: Runner,
    pub app: String,
    pub pick: WindowPick,
    pub step: Step,
    pub on_fail: Option<String>,
    pub max_attempts: u32,
}

impl StepTask {
    pub fn new(id: &str, runner: &Runner, app: &str, step: Step) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            runner: runner.clone(),
            app: app.into(),
            pick: WindowPick::default(),
            step,
            on_fail: None,
            max_attempts: 1,
        })
    }

    pub fn with_pick(mut self: Arc<Self>, pick: WindowPick) -> Arc<Self> {
        Arc::make_mut_or_clone(&mut self).pick = pick;
        self
    }

    pub fn recover_with(mut self: Arc<Self>, on_fail: &str, max_attempts: u32) -> Arc<Self> {
        let t = Arc::make_mut_or_clone(&mut self);
        t.on_fail = Some(on_fail.into());
        t.max_attempts = max_attempts.max(1);
        self
    }
}

trait ArcExt<T: Clone> {
    fn make_mut_or_clone(this: &mut Arc<T>) -> &mut T;
}
impl<T: Clone> ArcExt<T> for Arc<T> {
    fn make_mut_or_clone(this: &mut Arc<T>) -> &mut T {
        Arc::make_mut(this)
    }
}

impl Clone for StepTask {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            runner: self.runner.clone(),
            app: self.app.clone(),
            pick: self.pick.clone(),
            step: self.step.clone(),
            on_fail: self.on_fail.clone(),
            max_attempts: self.max_attempts,
        }
    }
}

#[async_trait]
impl Task for StepTask {
    fn id(&self) -> &str {
        &self.id
    }

    async fn run(&self, ctx: Context) -> graph_flow::Result<TaskResult> {
        let v = vars(&ctx);
        // The backend is synchronous and may block for seconds (VLM calls
        // via reqwest::blocking, settle sleeps): run it off the async thread.
        let (runner, app, pick, step) = (
            self.runner.clone(),
            self.app.clone(),
            self.pick.clone(),
            self.step.clone(),
        );
        let res = tokio::task::spawn_blocking(move || runner.step(&app, &pick, &step, &v))
            .await
            .map_err(|e| fail(format!("{}: join: {e}", self.id)))?;
        match res {
            Ok(reps) => {
                push_report(&ctx, &self.id, reps)?;
                Ok(TaskResult::new(None, NextAction::ContinueAndExecute))
            }
            Err(e) => {
                let key = format!("attempts:{}", self.id);
                let attempts: u32 = ctx.get(&key).unwrap_or(0) + 1;
                ctx.set(&key, attempts)?;
                match &self.on_fail {
                    Some(target) if attempts < self.max_attempts => Ok(TaskResult::new(
                        Some(format!("{}: {e}; retrying via {target}", self.id)),
                        NextAction::GoTo(target.clone()),
                    )),
                    _ => Err(fail(format!("{}: {e}", self.id))),
                }
            }
        }
    }
}

/// Screenshot + yes/no question; stores the boolean under `key` for a
/// conditional edge to read.
pub struct AskTask {
    pub id: String,
    pub runner: Runner,
    pub app: String,
    pub pick: WindowPick,
    pub question: String,
    pub key: String,
}

impl AskTask {
    pub fn new(id: &str, runner: &Runner, app: &str, question: &str, key: &str) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            runner: runner.clone(),
            app: app.into(),
            pick: WindowPick::default(),
            question: question.into(),
            key: key.into(),
        })
    }
}

#[async_trait]
impl Task for AskTask {
    fn id(&self) -> &str {
        &self.id
    }

    async fn run(&self, ctx: Context) -> graph_flow::Result<TaskResult> {
        let v = vars(&ctx);
        let (runner, app, pick, q) = (
            self.runner.clone(),
            self.app.clone(),
            self.pick.clone(),
            self.question.clone(),
        );
        let started = std::time::Instant::now();
        let (yes, answer) = tokio::task::spawn_blocking(move || runner.ask(&app, &pick, &q, &v))
            .await
            .map_err(|e| fail(format!("{}: join: {e}", self.id)))?
            .map_err(|e| fail(format!("{}: {e}", self.id)))?;
        ctx.set(&self.key, yes)?;
        let rep = StepReport {
            index: 0,
            step: format!("ask({})", self.question),
            resolved_by: if yes { "yes" } else { "no" }.into(),
            clicked: None,
            answer: Some(answer),
            ms: started.elapsed().as_millis(),
        };
        push_report(&ctx, &self.id, vec![rep])?;
        Ok(TaskResult::new(None, NextAction::ContinueAndExecute))
    }
}

pub struct EndTask;

#[async_trait]
impl Task for EndTask {
    fn id(&self) -> &str {
        "done"
    }
    async fn run(&self, _ctx: Context) -> graph_flow::Result<TaskResult> {
        Ok(TaskResult::new(None, NextAction::End))
    }
}

/// Drive a graph to completion (following `GoTo` pauses) and return the
/// accumulated step reports.
pub async fn run_graph(graph: Graph, vars: HashMap<String, String>) -> Result<Vec<StepReport>> {
    let start = graph
        .start_task_id()
        .ok_or_else(|| Error::internal("graph has no start task"))?
        .to_string();
    let storage = Arc::new(InMemorySessionStorage::new());
    let runner = FlowRunner::new(Arc::new(graph), storage.clone());
    let sid = format!(
        "sudohand-desktop-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let session = Session::new_from_task(sid.clone(), &start);
    session
        .context
        .set(VARS, &vars)
        .map_err(|e| Error::internal(format!("context: {e}")))?;
    storage
        .save(session)
        .await
        .map_err(|e| Error::internal(format!("session: {e}")))?;
    async fn report(storage: &InMemorySessionStorage, sid: &str) -> Vec<StepReport> {
        storage
            .get(sid)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.context.get::<Vec<StepReport>>(REPORT))
            .unwrap_or_default()
    }
    loop {
        match runner.run(&sid).await {
            Ok(r) => match r.status {
                ExecutionStatus::Completed => return Ok(report(&storage, &sid).await),
                ExecutionStatus::Paused { .. } => continue,
                ExecutionStatus::WaitingForInput => {
                    return Err(Error::internal("graph is waiting for input"))
                }
            },
            Err(e) => {
                let so_far = report(&storage, &sid).await;
                return Err(Error::io(format!(
                    "flow failed after {} steps: {e}",
                    so_far.len()
                )));
            }
        }
    }
}

/// Blocking convenience for CLIs: a current-thread runtime.
pub fn run_graph_blocking(graph: Graph, vars: HashMap<String, String>) -> Result<Vec<StepReport>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal(format!("tokio: {e}")))?;
    rt.block_on(run_graph(graph, vars))
}
