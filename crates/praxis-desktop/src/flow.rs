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
//! [`wechat_send`] builds the reference graph:
//!
//! ```text
//! ensure_chat ──chat_open──▶ focus_input ─▶ type_message ─▶ send ─▶ verify_sent ─▶ done
//!      │ else                     ▲
//!      ▼                          │
//! pick_from_list ────────────────┘  (the contact's row in the visible chat list)
//!      │ check fails: GoTo open_search (once)
//!      ▼
//! open_search ─▶ select_all ─▶ type_contact ─▶ wait ─▶ pick_result ──(check fails: GoTo open_search, ≤3)
//! ```

use crate::workflow::{Runner, Step, StepReport, WindowPick};
use async_trait::async_trait;
use graph_flow::{
    Context, ExecutionStatus, FlowRunner, Graph, GraphBuilder, GraphError, InMemorySessionStorage,
    NextAction, Session, SessionStorage, Task, TaskResult,
};
use praxis_core::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

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

/// The reference "send a WeChat message" graph. Variables: `contact`,
/// `message`.
pub fn wechat_send(runner: &Runner) -> Result<Graph> {
    let app = "com.tencent.xinWeChat";
    let ensure_chat = AskTask::new(
        "ensure_chat",
        runner,
        app,
        "这是微信截图。右侧聊天窗口顶部的标题是否为“{{contact}}”(后面可以带括号人数,如“{{contact}} (3)”)?只回答 yes 或 no",
        "chat_open",
    );
    let pick_from_list = StepTask::new(
        "pick_from_list",
        runner,
        app,
        Step::Click {
            at: None,
            find: Some("左侧会话列表(左边那一栏)中名为“{{contact}}”的那一行会话条目(头像右侧的名字正好是“{{contact}}”)".into()),
            check: Some("这是微信截图。右侧聊天窗口顶部的标题是否为“{{contact}}”(可带括号人数)?只回答 yes 或 no".into()),
            count: 1,
        },
    )
    .recover_with("open_search", 2);
    let open_search = StepTask::new(
        "open_search",
        runner,
        app,
        Step::Click {
            at: Some([200.0, 31.0]),
            find: Some("左侧顶部的 Search 搜索框".into()),
            check: None,
            count: 1,
        },
    );
    let select_all = StepTask::new(
        "select_all",
        runner,
        app,
        Step::Key {
            keys: "cmd+a".into(),
        },
    );
    let type_contact = StepTask::new(
        "type_contact",
        runner,
        app,
        Step::Type {
            text: "{{contact}}".into(),
        },
    );
    let wait_results = StepTask::new("wait_results", runner, app, Step::Wait { ms: 1500 });
    let pick_result = StepTask::new(
        "pick_result",
        runner,
        app,
        Step::Click {
            at: None,
            find: Some("左侧下拉搜索结果中、“群聊”或“联系人”分组下名为“{{contact}}”的那一条会话条目。注意:不要选最上面带放大镜的“搜一搜”网络搜索项".into()),
            check: Some("这是微信截图。右侧聊天窗口顶部的标题是否为“{{contact}}”(可带括号人数)?只回答 yes 或 no".into()),
            count: 1,
        },
    )
    .recover_with("open_search", 3);
    let focus_input = StepTask::new(
        "focus_input",
        runner,
        app,
        Step::Click {
            at: None,
            find: Some("右侧底部工具栏图标行(表情、文件夹、剪刀等图标)正下方约 20 像素处的空白消息输入区域".into()),
            check: Some("这是微信截图。右侧底部的消息输入框是否完全为空(没有任何已输入文字,也没有引用回复条)?只回答 yes 或 no".into()),
            count: 1,
        },
    );
    let type_message = StepTask::new(
        "type_message",
        runner,
        app,
        Step::Type {
            text: "{{message}}".into(),
        },
    );
    let send = StepTask::new(
        "send",
        runner,
        app,
        Step::Key {
            keys: "return".into(),
        },
    );
    let wait_sent = StepTask::new("wait_sent", runner, app, Step::Wait { ms: 1000 });
    let verify_sent = StepTask::new(
        "verify_sent",
        runner,
        app,
        Step::Verify {
            ask: "这是微信截图。把右侧聊天区域最下方那个绿色气泡(我方发送)里的文字原样转写出来,只输出文字本身".into(),
            expect: "{{message}}".into(),
        },
    );
    let done = Arc::new(EndTask);

    GraphBuilder::new("wechat_send")
        .add_task(ensure_chat.clone())
        .add_task(pick_from_list.clone())
        .add_task(open_search.clone())
        .add_task(select_all.clone())
        .add_task(type_contact.clone())
        .add_task(wait_results.clone())
        .add_task(pick_result.clone())
        .add_task(focus_input.clone())
        .add_task(type_message.clone())
        .add_task(send.clone())
        .add_task(wait_sent.clone())
        .add_task(verify_sent.clone())
        .add_task(done.clone())
        .set_start_task(ensure_chat.id())
        .add_conditional_edge(
            ensure_chat.id(),
            |ctx| ctx.get::<bool>("chat_open").unwrap_or(false),
            focus_input.id(),
            pick_from_list.id(),
        )
        .add_edge(pick_from_list.id(), focus_input.id())
        .add_edge(open_search.id(), select_all.id())
        .add_edge(select_all.id(), type_contact.id())
        .add_edge(type_contact.id(), wait_results.id())
        .add_edge(wait_results.id(), pick_result.id())
        .add_edge(pick_result.id(), focus_input.id())
        .add_edge(focus_input.id(), type_message.id())
        .add_edge(type_message.id(), send.id())
        .add_edge(send.id(), wait_sent.id())
        .add_edge(wait_sent.id(), verify_sent.id())
        .add_edge(verify_sent.id(), done.id())
        .with_max_execution_steps(60)
        .build()
        .map_err(|e| Error::internal(format!("graph: {e}")))
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
        "praxis-desktop-{}",
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
