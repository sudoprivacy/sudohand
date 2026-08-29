//! LangGraph-style orchestration (via [`graph_flow`]) over browser
//! [`Step`]s. Nodes are [`StepTask`] (run one step; on failure jump to a
//! recovery node up to `max_attempts` times, else abort) and [`EndTask`].
//! Shared state lives in the graph-flow [`Context`]: `vars` (the `{{name}}`
//! substitutions), `report` (every [`StepReport`] so far) and per-task
//! attempt counters.
//!
//! Two reference graphs ship: [`form_signup`] (fill and submit a form, then
//! verify the landing page) and [`page_extract`] (open a URL and evaluate an
//! expression).

// The ported adb modules are clippy-pedantic; the flow layer follows the
// workspace's standard clippy level like sudohand-desktop's.
#![allow(clippy::pedantic)]

use crate::workflow::{Runner, Step, StepReport};
use async_trait::async_trait;
use graph_flow::{
    Context, ExecutionStatus, FlowRunner, Graph, GraphBuilder, GraphError, InMemorySessionStorage,
    NextAction, Session, SessionStorage, Task, TaskResult,
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

fn push_report(ctx: &Context, task: &str, mut r: StepReport) -> graph_flow::Result<()> {
    let mut all: Vec<StepReport> = ctx.get(REPORT).unwrap_or_default();
    r.index = all.len();
    r.step = format!("{task}: {}", r.step);
    all.push(r);
    ctx.set(REPORT, all)
}

fn fail(msg: impl Into<String>) -> GraphError {
    GraphError::TaskExecutionFailed(msg.into())
}

/// Run one workflow step. On error, jump to `on_fail` while attempts remain;
/// otherwise the whole flow fails with the step's error.
#[derive(Clone)]
pub struct StepTask {
    pub id: String,
    pub runner: Runner,
    pub step: Step,
    pub on_fail: Option<String>,
    pub max_attempts: u32,
}

impl StepTask {
    pub fn new(id: &str, runner: &Runner, step: Step) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            runner: runner.clone(),
            step,
            on_fail: None,
            max_attempts: 1,
        })
    }

    pub fn recover_with(mut self: Arc<Self>, on_fail: &str, max_attempts: u32) -> Arc<Self> {
        let t = Arc::make_mut(&mut self);
        t.on_fail = Some(on_fail.into());
        t.max_attempts = max_attempts.max(1);
        self
    }
}

#[async_trait]
impl Task for StepTask {
    fn id(&self) -> &str {
        &self.id
    }

    async fn run(&self, ctx: Context) -> graph_flow::Result<TaskResult> {
        let v = vars(&ctx);
        match self.runner.step(&self.step, &v).await {
            Ok(rep) => {
                push_report(&ctx, &self.id, rep)?;
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

/// Build a straight-line graph from `(id, task)` pairs ending in `done`.
pub fn chain(name: &str, tasks: Vec<Arc<StepTask>>) -> Result<Graph> {
    let done = Arc::new(EndTask);
    let mut b = GraphBuilder::new(name);
    for t in &tasks {
        b = b.add_task(t.clone());
    }
    b = b.add_task(done.clone());
    if let Some(first) = tasks.first() {
        b = b.set_start_task(first.id());
    }
    for w in tasks.windows(2) {
        b = b.add_edge(w[0].id(), w[1].id());
    }
    if let Some(last) = tasks.last() {
        b = b.add_edge(last.id(), done.id());
    }
    b.with_max_execution_steps(100)
        .build()
        .map_err(|e| Error::internal(format!("graph: {e}")))
}

/// Reference graph: open `{{url}}`, fill `Username` / `Email`, submit with
/// the `Create account` button, wait for the result page and verify the
/// query string carries the username. If the submit click misses (page
/// still loading), it goes back to filling the form once more.
pub fn form_signup(runner: &Runner) -> Result<Graph> {
    let goto = StepTask::new(
        "open",
        runner,
        Step::Goto {
            url: "{{url}}".into(),
            wait: true,
        },
    );
    let username = StepTask::new(
        "username",
        runner,
        Step::TypeText {
            name: "Username".into(),
            text: "{{username}}".into(),
            clear: true,
            enter: false,
            timeout: 5.0,
        },
    );
    let email = StepTask::new(
        "email",
        runner,
        Step::TypeText {
            name: "Email".into(),
            text: "{{email}}".into(),
            clear: true,
            enter: false,
            timeout: 5.0,
        },
    );
    let submit = StepTask::new(
        "submit",
        runner,
        Step::ClickText {
            text: "Create account".into(),
            timeout: 5.0,
        },
    )
    .recover_with("username", 2);
    let landed = StepTask::new(
        "landed",
        runner,
        Step::WaitUrl {
            pattern: "result".into(),
            timeout: 10.0,
        },
    )
    .recover_with("username", 2);
    let verify = StepTask::new(
        "verify",
        runner,
        Step::Verify {
            expression: "document.getElementById('query').textContent".into(),
            expect: "username={{username}}".into(),
        },
    );
    chain(
        "form_signup",
        vec![goto, username, email, submit, landed, verify],
    )
}

/// Reference graph: open `{{url}}`, wait until the page is idle, evaluate
/// `{{expression}}` and return its value in the report.
pub fn page_extract(runner: &Runner) -> Result<Graph> {
    let goto = StepTask::new(
        "open",
        runner,
        Step::Goto {
            url: "{{url}}".into(),
            wait: true,
        },
    );
    let ready = StepTask::new("ready", runner, Step::WaitReady { timeout: 10.0 });
    let extract = StepTask::new(
        "extract",
        runner,
        Step::Js {
            expression: "{{expression}}".into(),
        },
    );
    chain("page_extract", vec![goto, ready, extract])
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
        "sudohand-browser-{}",
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
