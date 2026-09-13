use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::FutureExt;
use serde_json::{json, Map, Value};
use tokio::{sync::watch, task::JoinHandle};

use super::{
    load_state, save_state, Job, JobResult, JobStatus, PoolState, ProfileManager, ProfileMode,
    Worker, WorkerStatus,
};
use crate::{Error, Result};

pub type PoolFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ClientFactory =
    Arc<dyn Fn(WorkerOptions) -> PoolFuture<'static, Result<Box<dyn PoolClient>>> + Send + Sync>;
pub type FailCondition = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

/// The client owns its browser. Factories receive a unique worker port and its
/// cookie path; close must save cookies and honor whether Chrome should remain.
pub trait PoolClient: Send {
    fn execute<'a>(
        &'a mut self,
        job: &'a Job,
        context: JobContext,
    ) -> PoolFuture<'a, std::result::Result<ExecutionResult, JobFailure>>;
    fn close(&mut self, close_browser: bool) -> PoolFuture<'_, Result<()>>;

    /// Override UI pacing for one invocation. Return the previous value when
    /// supported so the pool can restore it, including after cancellation.
    fn replace_ui_delay(&mut self, _value: Value) -> Option<Value> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct WorkerOptions {
    pub worker_id: u64,
    pub port: u16,
    pub headless: bool,
    pub cookies_file: Option<PathBuf>,
    pub client_kwargs: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub struct ExecutionResult {
    pub data: Value,
    pub success: bool,
}

impl From<Value> for ExecutionResult {
    fn from(data: Value) -> Self {
        Self {
            data,
            success: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct JobFailure {
    pub message: String,
    pub error_type: Option<String>,
    pub error_bases: Vec<String>,
}

impl JobFailure {
    #[must_use]
    pub fn new(message: impl Into<String>, error_type: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_type: Some(error_type.into()),
            error_bases: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum RequeuePosition {
    Front,
    #[default]
    Back,
}

pub struct PoolOptions {
    pub workers: usize,
    pub max_retries: i32,
    pub state_file: Option<PathBuf>,
    pub headless: bool,
    pub close_browsers: bool,
    pub profile: ProfileMode,
    pub cookies_file: Option<PathBuf>,
    pub cookies_dir: Option<PathBuf>,
    pub client_kwargs: Map<String, Value>,
    pub requeue_position: RequeuePosition,
    pub fail_condition: Option<FailCondition>,
}

impl Default for PoolOptions {
    fn default() -> Self {
        Self {
            workers: 3,
            max_retries: 2,
            state_file: None,
            headless: false,
            close_browsers: true,
            profile: ProfileMode::Shared,
            cookies_file: None,
            cookies_dir: None,
            client_kwargs: Map::new(),
            requeue_position: RequeuePosition::Back,
            fail_condition: None,
        }
    }
}

#[derive(Default)]
struct SharedTarget {
    target: u64,
    counts: BTreeMap<String, u64>,
}

struct State {
    running: bool,
    next_worker: u64,
    jobs: BTreeMap<String, Job>,
    queue: VecDeque<String>,
    priority: VecDeque<String>,
    held: Vec<String>,
    results: BTreeMap<String, JobResult>,
    workers: BTreeMap<u64, Worker>,
    shared: BTreeMap<String, Arc<Mutex<SharedTarget>>>,
    selecting: BTreeSet<String>,
    checkpoint_error: Option<String>,
}

struct Inner {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    options: PoolOptions,
    profiles: ProfileManager,
}

impl Inner {
    fn changed(&self) {
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    fn snapshot(state: &State) -> PoolState {
        let pending = state
            .priority
            .iter()
            .chain(state.queue.iter())
            .chain(state.held.iter())
            .filter_map(|id| state.jobs.get(id).cloned())
            .collect();
        let in_progress = state
            .jobs
            .values()
            .filter(|job| job.status == JobStatus::InProgress)
            .cloned()
            .collect();
        PoolState {
            completed: state.results.clone(),
            pending,
            in_progress,
            ..PoolState::default()
        }
    }

    fn write_checkpoint(&self, state: &mut State) -> Result<()> {
        let Some(path) = &self.options.state_file else {
            return Ok(());
        };
        let result = save_state(&mut Self::snapshot(state), path);
        state.checkpoint_error = result.as_ref().err().map(ToString::to_string);
        result
    }

    fn checkpoint(&self) -> Result<()> {
        let result = self.write_checkpoint(&mut self.state.lock().expect("pool state"));
        self.changed();
        result
    }
}

/// Cooperative progress and selection hooks passed to every client invocation.
#[derive(Clone)]
pub struct JobContext {
    inner: Arc<Inner>,
    job_id: String,
}

impl JobContext {
    /// Report this job's cumulative success count. False tells a client to stop
    /// producing work because the shared target has been reached.
    #[must_use]
    pub fn progress(&self, current_success: u64) -> bool {
        let shared = self
            .inner
            .state
            .lock()
            .expect("pool state")
            .shared
            .get(&self.job_id)
            .cloned();
        let Some(shared) = shared else {
            return true;
        };
        let mut shared = shared.lock().expect("shared target");
        shared.counts.insert(self.job_id.clone(), current_success);
        shared.counts.values().sum::<u64>() < shared.target
    }

    #[must_use]
    pub fn min_success(&self) -> Option<u64> {
        let state = self.inner.state.lock().expect("pool state");
        if let Some(value) = state
            .jobs
            .get(&self.job_id)
            .and_then(|job| job.kwargs.get("min_success"))
        {
            return value.as_u64();
        }
        state
            .shared
            .get(&self.job_id)
            .map(|shared| shared.lock().expect("shared target").target)
    }

    /// Keep wait timeouts from cutting off a user selection. Dropping this guard,
    /// including on cancellation or panic, always leaves the selection phase.
    #[must_use]
    pub fn selection(&self) -> SelectionGuard {
        self.inner
            .state
            .lock()
            .expect("pool state")
            .selecting
            .insert(self.job_id.clone());
        self.inner.changed();
        SelectionGuard {
            context: self.clone(),
        }
    }
}

pub struct SelectionGuard {
    context: JobContext,
}
impl Drop for SelectionGuard {
    fn drop(&mut self) {
        self.context
            .inner
            .state
            .lock()
            .expect("pool state")
            .selecting
            .remove(&self.context.job_id);
        self.context.inner.changed();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Running,
    Graceful,
    Cancel,
}
struct Control {
    stop: watch::Sender<Stop>,
    task: JoinHandle<Result<()>>,
}

pub struct BrowserPool {
    inner: Arc<Inner>,
    factory: ClientFactory,
    controls: Mutex<BTreeMap<u64, Control>>,
    lifecycle: tokio::sync::Mutex<()>,
}

impl BrowserPool {
    pub async fn start(factory: ClientFactory, options: PoolOptions) -> Result<Self> {
        if options.max_retries < -1 {
            return Err(Error::Invalid(
                "max_retries must be -1 or nonnegative".into(),
            ));
        }
        let mut saved = match &options.state_file {
            Some(path) => load_state(path)?.unwrap_or_default(),
            None => PoolState::default(),
        };
        saved.resume();
        let queue = saved.pending.iter().map(|job| job.job_id.clone()).collect();
        let jobs = saved
            .pending
            .into_iter()
            .map(|job| (job.job_id.clone(), job))
            .collect();
        let profiles = ProfileManager::new(
            options.profile,
            options.cookies_file.clone(),
            options.cookies_dir.clone(),
        )?;
        let workers = options.workers;
        let (changed, _) = watch::channel(0);
        let pool = Self {
            inner: Arc::new(Inner {
                options,
                profiles,
                changed,
                state: Mutex::new(State {
                    running: true,
                    next_worker: 0,
                    jobs,
                    queue,
                    priority: VecDeque::new(),
                    held: Vec::new(),
                    results: saved.completed,
                    workers: BTreeMap::new(),
                    shared: BTreeMap::new(),
                    selecting: BTreeSet::new(),
                    checkpoint_error: None,
                }),
            }),
            factory,
            controls: Mutex::new(BTreeMap::new()),
            lifecycle: tokio::sync::Mutex::new(()),
        };
        for _ in 0..workers {
            if let Err(error) = pool.add_worker().await {
                let _ = pool.shutdown(false).await;
                return Err(error);
            }
        }
        Ok(pool)
    }

    pub async fn add_worker(&self) -> Result<u64> {
        let _lifecycle = self.lifecycle.lock().await;
        let (id, port) = {
            let mut state = self.inner.state.lock().expect("pool state");
            if !state.running {
                return Err(Error::Invalid("pool is stopped".into()));
            }
            let ports = state
                .workers
                .values()
                .map(|worker| worker.port)
                .collect::<Vec<_>>();
            let port = crate::port::get_available_port(crate::config::DEFAULT_PORT_RANGE, &ports)?;
            let id = state.next_worker;
            state.next_worker += 1;
            state.workers.insert(id, Worker::new(id, port));
            (id, port)
        };
        let client = (self.factory)(WorkerOptions {
            worker_id: id,
            port,
            headless: self.inner.options.headless,
            cookies_file: self.inner.profiles.get_cookies_file(id),
            client_kwargs: self.inner.options.client_kwargs.clone(),
        })
        .await;
        let client = match client {
            Ok(client) => client,
            Err(error) => {
                self.inner
                    .state
                    .lock()
                    .expect("pool state")
                    .workers
                    .remove(&id);
                self.inner.changed();
                return Err(error);
            }
        };
        let mut client = Some(client);
        let started = {
            let mut controls = self.controls.lock().expect("worker controls");
            let running = self.inner.state.lock().expect("pool state").running;
            if running {
                let (stop, receiver) = watch::channel(Stop::Running);
                let inner = self.inner.clone();
                let client = client.take().expect("new client");
                let task =
                    tokio::spawn(async move { worker_loop(inner, id, client, receiver).await });
                controls.insert(id, Control { stop, task });
            }
            running
        };
        if !started {
            self.inner
                .state
                .lock()
                .expect("pool state")
                .workers
                .remove(&id);
            client
                .as_mut()
                .expect("unstarted client")
                .close(self.inner.options.close_browsers)
                .await?;
            return Err(Error::Invalid(
                "pool stopped during worker initialization".into(),
            ));
        }
        self.inner.changed();
        Ok(id)
    }

    pub async fn remove_worker(&self, id: u64, wait: bool) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let control = self
            .controls
            .lock()
            .expect("worker controls")
            .remove(&id)
            .ok_or_else(|| Error::Invalid(format!("unknown worker {id}")))?;
        if let Some(worker) = self
            .inner
            .state
            .lock()
            .expect("pool state")
            .workers
            .get_mut(&id)
        {
            worker.status = WorkerStatus::Stopping;
        }
        let _ = control
            .stop
            .send(if wait { Stop::Graceful } else { Stop::Cancel });
        self.inner.changed();
        let result = control
            .task
            .await
            .map_err(|error| Error::Invalid(format!("worker task: {error}")))
            .and_then(std::convert::identity);
        self.inner
            .state
            .lock()
            .expect("pool state")
            .workers
            .remove(&id);
        self.inner.changed();
        result
    }

    pub fn run(
        &self,
        task_type: impl Into<String>,
        args: Vec<Value>,
        kwargs: Map<String, Value>,
        max_retries: Option<i32>,
        hold: bool,
    ) -> Result<String> {
        let mut job = Job::new(task_type);
        job.args = args;
        job.kwargs = kwargs;
        job.max_retries = max_retries.unwrap_or(self.inner.options.max_retries);
        if job.max_retries < -1 {
            return Err(Error::Invalid(
                "max_retries must be -1 or nonnegative".into(),
            ));
        }
        let id = job.job_id.clone();
        let mut state = self.inner.state.lock().expect("pool state");
        if !state.running {
            return Err(Error::Invalid("pool is stopped".into()));
        }
        state.jobs.insert(id.clone(), job);
        if hold {
            state.held.push(id.clone());
        } else {
            state.queue.push_back(id.clone());
        }
        if let Err(error) = self.inner.write_checkpoint(&mut state) {
            state.jobs.remove(&id);
            state.queue.retain(|queued| queued != &id);
            state.held.retain(|held| held != &id);
            drop(state);
            self.inner.changed();
            return Err(error);
        }
        drop(state);
        self.inner.changed();
        Ok(id)
    }

    pub fn submit(
        &self,
        task_type: impl Into<String>,
        args: Vec<Value>,
        kwargs: Map<String, Value>,
        max_retries: Option<i32>,
        hold: bool,
    ) -> Result<String> {
        self.run(task_type, args, kwargs, max_retries, hold)
    }

    pub fn submit_batch(
        &self,
        jobs: Vec<(String, Vec<Value>, Map<String, Value>)>,
    ) -> Result<Vec<String>> {
        jobs.into_iter()
            .map(|(kind, args, kwargs)| self.run(kind, args, kwargs, None, false))
            .collect()
    }

    #[must_use]
    pub fn get_result(&self, id: &str) -> Option<JobResult> {
        self.inner
            .state
            .lock()
            .expect("pool state")
            .results
            .get(id)
            .cloned()
    }

    pub fn save_state(&self) -> Result<()> {
        self.inner.checkpoint()
    }

    /// Unfinished jobs, counted once even when they are also queued.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.inner.state.lock().expect("pool state").jobs.len()
    }

    #[must_use]
    pub fn completed_count(&self) -> usize {
        self.inner.state.lock().expect("pool state").results.len()
    }

    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.inner.state.lock().expect("pool state").workers.len()
    }

    #[must_use]
    pub fn workers(&self) -> BTreeMap<u64, Worker> {
        self.inner.state.lock().expect("pool state").workers.clone()
    }

    #[must_use]
    pub fn worker_stats(&self) -> BTreeMap<u64, super::WorkerStats> {
        self.workers()
            .into_iter()
            .map(|(id, worker)| (id, worker.stats))
            .collect()
    }

    pub async fn wait_current_task(
        &self,
        worker_id: u64,
        timeout: Option<Duration>,
    ) -> Result<bool> {
        let mut changed = self.inner.changed.subscribe();
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            {
                let state = self.inner.state.lock().expect("pool state");
                let worker = state
                    .workers
                    .get(&worker_id)
                    .ok_or_else(|| Error::Invalid(format!("unknown worker {worker_id}")))?;
                if worker.current_job.is_none() {
                    return Ok(true);
                }
            }
            match wait_change(&mut changed, deadline).await {
                Err(Error::Timeout { .. }) => return Ok(false),
                result => result?,
            }
        }
    }

    pub async fn wait_for(&self, id: &str, timeout: Option<Duration>) -> Result<JobResult> {
        let mut changed = self.inner.changed.subscribe();
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            {
                let state = self.inner.state.lock().expect("pool state");
                if let Some(result) = state.results.get(id) {
                    return Ok(result.clone());
                }
                if !state.jobs.contains_key(id) {
                    return Err(Error::Invalid(format!("unknown job {id}")));
                }
            }
            wait_change(&mut changed, deadline).await?;
        }
    }

    pub async fn wait(
        &self,
        ids: Option<&[String]>,
        min_success: Option<u64>,
        timeout: Option<Duration>,
    ) -> Result<BTreeMap<String, JobResult>> {
        if min_success.is_some() && ids.is_none() {
            return Err(Error::Invalid("min_success requires job_ids".into()));
        }
        let mut changed = self.inner.changed.subscribe();
        {
            let mut state = self.inner.state.lock().expect("pool state");
            if let (Some(ids), Some(target)) = (ids, min_success) {
                let shared = Arc::new(Mutex::new(SharedTarget {
                    target,
                    ..SharedTarget::default()
                }));
                for id in ids {
                    state.shared.insert(id.clone(), shared.clone());
                }
            }
            let held = std::mem::take(&mut state.held);
            state.queue.extend(held);
        }
        self.inner.changed();
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            let selecting = {
                let state = self.inner.state.lock().expect("pool state");
                if let Some(error) = &state.checkpoint_error {
                    return Err(Error::Invalid(format!("pool checkpoint failed: {error}")));
                }
                let results: BTreeMap<_, _> = state
                    .results
                    .iter()
                    .filter(|(id, _)| ids.is_none_or(|ids| ids.contains(id)))
                    .map(|(id, result)| (id.clone(), result.clone()))
                    .collect();
                let all = ids.map_or_else(
                    || state.jobs.is_empty(),
                    |ids| ids.iter().all(|id| results.contains_key(id)),
                );
                let count: u64 = results
                    .values()
                    .filter(|result| result.success)
                    .map(|result| result.data["success_count"].as_u64().unwrap_or(0))
                    .sum();
                if all
                    || (min_success.is_some_and(|target| count >= target)
                        && state.selecting.is_empty())
                {
                    return Ok(results);
                }
                !state.selecting.is_empty()
            };
            wait_change(&mut changed, if selecting { None } else { deadline }).await?;
        }
    }

    #[must_use]
    pub fn get_status(&self) -> Value {
        let state = self.inner.state.lock().expect("pool state");
        json!({"running":state.running, "workers": state.workers.iter().map(|(id, worker)| (id.to_string(), worker.to_dict())).collect::<BTreeMap<_,_>>(),
            "pending_jobs":state.jobs.len(), "queue_size":state.queue.len(), "priority_queue_size":state.priority.len(),
            "completed_jobs":state.results.len(), "success_count":state.results.values().filter(|r|r.success).count(),
            "fail_count":state.results.values().filter(|r|!r.success).count()})
    }

    pub async fn shutdown(&self, graceful: bool) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.inner.state.lock().expect("pool state").running = false;
        let controls = std::mem::take(&mut *self.controls.lock().expect("worker controls"));
        for control in controls.values() {
            let _ = control.stop.send(if graceful {
                Stop::Graceful
            } else {
                Stop::Cancel
            });
        }
        self.inner.changed();
        let mut error = None;
        for (_, control) in controls {
            match control.task.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => error = Some(e),
                Err(e) => error = Some(Error::Invalid(e.to_string())),
            }
        }
        self.inner.state.lock().expect("pool state").workers.clear();
        self.inner.checkpoint()?;
        error.map_or(Ok(()), Err)
    }
}

impl Drop for BrowserPool {
    fn drop(&mut self) {
        for control in self.controls.lock().expect("worker controls").values() {
            let _ = control.stop.send(Stop::Cancel);
        }
    }
}

async fn wait_change(
    changed: &mut watch::Receiver<u64>,
    deadline: Option<tokio::time::Instant>,
) -> Result<()> {
    if let Some(deadline) = deadline {
        tokio::time::timeout_at(deadline, changed.changed())
            .await
            .map_err(|_| Error::Timeout {
                method: "pool.wait".into(),
                seconds: 0.0,
            })?
            .map_err(|_| Error::Invalid("pool stopped".into()))?;
    } else {
        changed
            .changed()
            .await
            .map_err(|_| Error::Invalid("pool stopped".into()))?;
    }
    Ok(())
}

async fn cancellation(stop: &mut watch::Receiver<Stop>) {
    loop {
        if *stop.borrow_and_update() == Stop::Cancel || stop.changed().await.is_err() {
            return;
        }
    }
}

async fn worker_loop(
    inner: Arc<Inner>,
    id: u64,
    mut client: Box<dyn PoolClient>,
    mut stop: watch::Receiver<Stop>,
) -> Result<()> {
    let mut changed = inner.changed.subscribe();
    loop {
        let job = {
            let mut state = inner.state.lock().expect("pool state");
            if !state.running || *stop.borrow() != Stop::Running {
                break;
            }
            let next = state
                .priority
                .pop_front()
                .or_else(|| state.queue.pop_front());
            next.and_then(|key| state.jobs.get_mut(&key))
                .map(|job| {
                    job.status = JobStatus::InProgress;
                    job.clone()
                })
                .inspect(|job| {
                    let worker = state.workers.get_mut(&id).expect("worker exists");
                    worker.current_job = Some(job.clone());
                    worker.status = WorkerStatus::Busy;
                })
        };
        let Some(mut job) = job else {
            tokio::select! { _ = changed.changed() => {}, _ = stop.changed() => {} }
            continue;
        };
        let _ = inner.checkpoint();
        inner.changed();
        let started = Instant::now();
        let context = JobContext {
            inner: inner.clone(),
            job_id: job.job_id.clone(),
        };
        let mut invocation = job.clone();
        let previous_delay = invocation
            .kwargs
            .remove("ui_delay")
            .filter(|value| !value.is_null())
            .and_then(|value| client.replace_ui_delay(value));
        let result = tokio::select! {
            biased;
            () = cancellation(&mut stop) => None,
            result = std::panic::AssertUnwindSafe(client.execute(&invocation, context)).catch_unwind() => {
                Some(result.unwrap_or_else(|_| Err(JobFailure::new("client execution panicked", "Panic"))))
            }
        };
        if let Some(previous) = previous_delay {
            client.replace_ui_delay(previous);
        }
        let result = result.map(|result| {
            result.and_then(|result| {
                let Some(condition) = &inner.options.fail_condition else {
                    return Ok(result);
                };
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    condition(&result.data)
                })) {
                    Ok(false) => Ok(result),
                    Ok(true) => Err(JobFailure {
                        message: "fail_condition returned true".into(),
                        error_type: None,
                        error_bases: Vec::new(),
                    }),
                    Err(_) => Err(JobFailure::new("fail_condition panicked", "Panic")),
                }
            })
        });
        {
            let mut state = inner.state.lock().expect("pool state");
            let worker = state.workers.get_mut(&id).expect("worker exists");
            worker.current_job = None;
            worker.status = WorkerStatus::Idle;
            match result {
                None => {
                    job.status = JobStatus::Pending;
                    state.queue.push_front(job.job_id.clone());
                    state.jobs.insert(job.job_id.clone(), job);
                }
                Some(Ok(outcome)) => {
                    worker.stats.success += 1;
                    worker.stats.total_time += started.elapsed().as_secs_f64();
                    let mut result = JobResult::success(&job.job_id, outcome.data, Some(id));
                    result.success = outcome.success;
                    state.jobs.remove(&job.job_id);
                    state.results.insert(job.job_id.clone(), result);
                }
                Some(Err(error)) => {
                    worker.stats.fail += 1;
                    job.retries = job.retries.saturating_add(1);
                    if job.can_retry() {
                        job.status = JobStatus::Pending;
                        match inner.options.requeue_position {
                            RequeuePosition::Front => state.priority.push_back(job.job_id.clone()),
                            RequeuePosition::Back => state.queue.push_back(job.job_id.clone()),
                        }
                        state.jobs.insert(job.job_id.clone(), job);
                    } else {
                        let mut result = JobResult::success(&job.job_id, Value::Null, Some(id));
                        result.success = false;
                        result.error = Some(error.message);
                        result.error_type = error.error_type;
                        result.error_bases = error.error_bases;
                        state.jobs.remove(&job.job_id);
                        state.results.insert(job.job_id.clone(), result);
                    }
                }
            }
        }
        let _ = inner.checkpoint();
        inner.changed();
    }
    let result = client.close(inner.options.close_browsers).await;
    if let Some(worker) = inner.state.lock().expect("pool state").workers.get_mut(&id) {
        worker.status = WorkerStatus::Stopped;
    }
    inner.changed();
    result
}
