use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{json, Map};
use sudohand_browser::{pool::*, Result};
use tokio::sync::{Notify, Semaphore};

struct Events {
    calls: Mutex<Vec<String>>,
    opened: Mutex<Vec<WorkerOptions>>,
    closed: Mutex<Vec<(u64, bool)>>,
    started: Notify,
    gate: Semaphore,
}

impl Default for Events {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            opened: Mutex::new(Vec::new()),
            closed: Mutex::new(Vec::new()),
            started: Notify::new(),
            gate: Semaphore::new(0),
        }
    }
}

struct Client {
    id: u64,
    events: Arc<Events>,
}

impl PoolClient for Client {
    fn execute<'a>(
        &'a mut self,
        job: &'a Job,
        context: JobContext,
    ) -> PoolFuture<'a, std::result::Result<ExecutionResult, JobFailure>> {
        Box::pin(async move {
            self.events
                .calls
                .lock()
                .unwrap()
                .push(format!("{}:{}", job.task_type, job.retries));
            match job.task_type.as_str() {
                "retry" if job.retries == 0 => Err(JobFailure::new("retry fixture", "Transient")),
                "fail" => Err(JobFailure {
                    message: "terminal fixture".into(),
                    error_type: Some("Timeout".into()),
                    error_bases: vec!["Transient".into()],
                }),
                "panic" => panic!("executor panic fixture"),
                "business_false" => Ok(ExecutionResult {
                    data: json!({"reason":"not found"}),
                    success: false,
                }),
                "gate" => {
                    self.events.started.notify_one();
                    self.events.gate.acquire().await.unwrap().forget();
                    Ok(json!({"done":true}).into())
                }
                "progress" => {
                    let mut count = 0;
                    while count < 20 {
                        count += 1;
                        if !context.progress(count) {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Ok(json!({"success_count":count, "target":context.min_success()}).into())
                }
                "selection" => {
                    let _selection = context.selection();
                    self.events.started.notify_one();
                    self.events.gate.acquire().await.unwrap().forget();
                    Ok(json!({"success_count":1}).into())
                }
                _ => Ok(json!({"done":true}).into()),
            }
        })
    }

    fn close(&mut self, close_browser: bool) -> PoolFuture<'_, Result<()>> {
        Box::pin(async move {
            self.events
                .closed
                .lock()
                .unwrap()
                .push((self.id, close_browser));
            Ok(())
        })
    }
}

fn fixture() -> (Arc<Events>, ClientFactory) {
    let events = Arc::new(Events {
        gate: Semaphore::new(0),
        ..Events::default()
    });
    let captured = events.clone();
    let factory: ClientFactory = Arc::new(move |options| {
        let events = captured.clone();
        Box::pin(async move {
            let id = options.worker_id;
            events.opened.lock().unwrap().push(options);
            Ok(Box::new(Client { id, events }) as Box<dyn PoolClient>)
        })
    });
    (events, factory)
}

fn options(workers: usize) -> PoolOptions {
    PoolOptions {
        workers,
        profile: ProfileMode::Temp,
        ..PoolOptions::default()
    }
}
const TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::test]
async fn retry_order_held_jobs_and_error_identity() {
    for (position, expected) in [
        (
            RequeuePosition::Front,
            vec!["retry:0", "retry:1", "success:0"],
        ),
        (
            RequeuePosition::Back,
            vec!["retry:0", "success:0", "retry:1"],
        ),
    ] {
        let (events, factory) = fixture();
        let pool = BrowserPool::start(
            factory,
            PoolOptions {
                requeue_position: position,
                ..options(1)
            },
        )
        .await
        .unwrap();
        let a = pool
            .run("retry", vec![], Map::new(), Some(2), true)
            .unwrap();
        let b = pool.run("success", vec![], Map::new(), None, true).unwrap();
        tokio::task::yield_now().await;
        assert!(events.calls.lock().unwrap().is_empty());
        let results = pool.wait(None, None, Some(TIMEOUT)).await.unwrap();
        assert!(results[&a].success && results[&b].success);
        assert_eq!(*events.calls.lock().unwrap(), expected);
        let failed = pool
            .run("fail", vec![], Map::new(), Some(1), false)
            .unwrap();
        let failure = pool.wait_for(&failed, Some(TIMEOUT)).await.unwrap();
        assert!(!failure.success);
        assert_eq!(failure.error_type.as_deref(), Some("Timeout"));
        assert_eq!(failure.error_bases, ["Transient"]);
        let panicked = pool
            .run("panic", vec![], Map::new(), Some(1), false)
            .unwrap();
        assert_eq!(
            pool.wait_for(&panicked, Some(TIMEOUT))
                .await
                .unwrap()
                .error_type
                .as_deref(),
            Some("Panic")
        );
        pool.shutdown(true).await.unwrap();
        assert_eq!(*events.closed.lock().unwrap(), [(0, true)]);
    }
}

#[tokio::test]
async fn cancelled_job_is_durable_and_resumes_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let (events, factory) = fixture();
    let pool = BrowserPool::start(
        factory,
        PoolOptions {
            state_file: Some(path.clone()),
            ..options(1)
        },
    )
    .await
    .unwrap();
    let id = pool.run("gate", vec![], Map::new(), None, false).unwrap();
    tokio::time::timeout(TIMEOUT, events.started.notified())
        .await
        .unwrap();
    pool.shutdown(false).await.unwrap();
    let saved = load_state(&path).unwrap().unwrap();
    assert_eq!(saved.pending.len(), 1);
    assert!(saved.in_progress.is_empty());
    assert_eq!(saved.pending[0].job_id, id);
    assert_eq!(saved.pending[0].retries, 0);
    let (events, factory) = fixture();
    events.gate.add_permits(1);
    let pool = BrowserPool::start(
        factory,
        PoolOptions {
            state_file: Some(path.clone()),
            ..options(1)
        },
    )
    .await
    .unwrap();
    assert!(pool.wait_for(&id, Some(TIMEOUT)).await.unwrap().success);
    pool.shutdown(true).await.unwrap();
    assert_eq!(*events.calls.lock().unwrap(), ["gate:0"]);
    let saved = load_state(&path).unwrap().unwrap();
    assert!(saved.pending.is_empty() && saved.in_progress.is_empty());
    assert!(saved.completed[&id].success);
}

#[tokio::test]
async fn scaling_graceful_removal_and_cookie_paths() {
    let dir = tempfile::tempdir().unwrap();
    let (events, factory) = fixture();
    let pool = Arc::new(
        BrowserPool::start(
            factory,
            PoolOptions {
                profile: ProfileMode::PerWorker,
                cookies_dir: Some(dir.path().into()),
                close_browsers: false,
                ..options(1)
            },
        )
        .await
        .unwrap(),
    );
    let id = pool.run("gate", vec![], Map::new(), None, false).unwrap();
    tokio::time::timeout(TIMEOUT, events.started.notified())
        .await
        .unwrap();
    let removing = pool.clone();
    let task = tokio::spawn(async move { removing.remove_worker(0, true).await });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    events.gate.add_permits(1);
    tokio::time::timeout(TIMEOUT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(pool.get_result(&id).unwrap().success);
    assert_eq!(pool.add_worker().await.unwrap(), 1);
    let opened = events.opened.lock().unwrap().clone();
    assert_eq!(
        opened[0].cookies_file,
        Some(dir.path().join("cookies_worker_0.dat"))
    );
    assert_eq!(
        opened[1].cookies_file,
        Some(dir.path().join("cookies_worker_1.dat"))
    );
    pool.shutdown(true).await.unwrap();
    assert_eq!(*events.closed.lock().unwrap(), [(0, false), (1, false)]);
}

#[tokio::test]
async fn shared_progress_and_wait_validation() {
    let (_, factory) = fixture();
    let pool = BrowserPool::start(factory, options(2)).await.unwrap();
    assert!(pool.wait(None, Some(2), Some(TIMEOUT)).await.is_err());
    assert!(pool.wait_for("missing", Some(TIMEOUT)).await.is_err());
    let ids: Vec<_> = (0..2)
        .map(|_| {
            pool.run("progress", vec![], Map::new(), None, true)
                .unwrap()
        })
        .collect();
    let results = pool.wait(Some(&ids), Some(5), Some(TIMEOUT)).await.unwrap();
    assert!(
        results
            .values()
            .map(|r| r.data["success_count"].as_u64().unwrap())
            .sum::<u64>()
            >= 5
    );
    for result in results.values() {
        assert_eq!(result.data["target"], 5);
    }
    pool.wait(None, None, Some(TIMEOUT)).await.unwrap();
    pool.shutdown(true).await.unwrap();
}

#[tokio::test]
async fn selection_suspends_timeout_until_guard_is_dropped() {
    let (events, factory) = fixture();
    let pool = Arc::new(BrowserPool::start(factory, options(1)).await.unwrap());
    let id = pool
        .run("selection", vec![], Map::new(), None, false)
        .unwrap();
    tokio::time::timeout(TIMEOUT, events.started.notified())
        .await
        .unwrap();
    let waiting = pool.clone();
    let task =
        tokio::spawn(async move { waiting.wait(Some(&[id]), None, Some(Duration::ZERO)).await });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    events.gate.add_permits(1);
    assert_eq!(
        tokio::time::timeout(TIMEOUT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    pool.shutdown(true).await.unwrap();
}

#[tokio::test]
async fn failed_checkpoint_does_not_submit_invisible_work() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("checkpoint");
    std::fs::create_dir(&parent).unwrap();
    let (events, factory) = fixture();
    let pool = BrowserPool::start(
        factory,
        PoolOptions {
            state_file: Some(parent.join("state.json")),
            ..options(1)
        },
    )
    .await
    .unwrap();
    std::fs::remove_dir(&parent).unwrap();
    std::fs::write(&parent, "occupied").unwrap();
    assert!(pool
        .run("success", vec![], Map::new(), None, false)
        .is_err());
    tokio::task::yield_now().await;
    assert!(events.calls.lock().unwrap().is_empty());
    assert_eq!(pool.pending_count(), 0);
    std::fs::remove_file(&parent).unwrap();
    std::fs::create_dir(&parent).unwrap();
    pool.save_state().unwrap();
    let id = pool
        .run("success", vec![], Map::new(), None, false)
        .unwrap();
    assert!(pool.wait_for(&id, Some(TIMEOUT)).await.unwrap().success);
    pool.shutdown(true).await.unwrap();
}

#[tokio::test]
async fn failure_policy_and_business_outcomes_are_distinct() {
    let (events, factory) = fixture();
    let pool = BrowserPool::start(
        factory,
        PoolOptions {
            max_retries: 1,
            fail_condition: Some(Arc::new(|value| value["done"] == true)),
            ..options(1)
        },
    )
    .await
    .unwrap();
    let rejected = pool
        .submit("success", vec![], Map::new(), None, false)
        .unwrap();
    let result = pool.wait_for(&rejected, Some(TIMEOUT)).await.unwrap();
    assert!(!result.success);
    assert!(result.error.is_some() && result.error_type.is_none() && result.error_bases.is_empty());
    let business = pool
        .submit("business_false", vec![], Map::new(), None, false)
        .unwrap();
    let result = pool.wait_for(&business, Some(TIMEOUT)).await.unwrap();
    assert!(!result.success && result.error.is_none());
    assert_eq!(result.data["reason"], "not found");
    assert_eq!(pool.worker_stats()[&0].success, 1);
    assert_eq!(pool.worker_stats()[&0].fail, 1);
    assert_eq!(pool.completed_count(), 2);
    assert_eq!(events.calls.lock().unwrap().len(), 2);
    pool.shutdown(true).await.unwrap();
}

#[tokio::test]
async fn partial_start_failure_closes_already_created_clients() {
    let (events, delegate) = fixture();
    let factory: ClientFactory = Arc::new(move |options| {
        let delegate = delegate.clone();
        Box::pin(async move {
            if options.worker_id == 1 {
                return Err(sudohand_browser::Error::Invalid(
                    "factory fixture failed".into(),
                ));
            }
            delegate(options).await
        })
    });
    assert!(BrowserPool::start(factory, options(2)).await.is_err());
    assert_eq!(*events.closed.lock().unwrap(), [(0, true)]);
}

#[tokio::test]
async fn invocation_overrides_are_scoped_even_on_failure_and_cancellation() {
    struct PacingClient {
        delay: serde_json::Value,
        events: Arc<Mutex<Vec<serde_json::Value>>>,
        started: Arc<Notify>,
    }
    impl PoolClient for PacingClient {
        fn replace_ui_delay(&mut self, value: serde_json::Value) -> Option<serde_json::Value> {
            Some(std::mem::replace(&mut self.delay, value))
        }
        fn execute<'a>(
            &'a mut self,
            job: &'a Job,
            context: JobContext,
        ) -> PoolFuture<'a, std::result::Result<ExecutionResult, JobFailure>> {
            Box::pin(async move {
                assert!(!job.kwargs.contains_key("ui_delay"));
                self.events.lock().unwrap().push(self.delay.clone());
                match job.task_type.as_str() {
                    "panic" => panic!("pacing fixture"),
                    "cancel" => {
                        self.started.notify_one();
                        std::future::pending().await
                    }
                    _ => Ok(json!({"target":context.min_success()}).into()),
                }
            })
        }
        fn close(&mut self, _: bool) -> PoolFuture<'_, Result<()>> {
            Box::pin(async move {
                assert_eq!(self.delay, json!([1, 2]));
                Ok(())
            })
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(Notify::new());
    let factory: ClientFactory = {
        let events = events.clone();
        let started = started.clone();
        Arc::new(move |_| {
            let client = PacingClient {
                delay: json!([1, 2]),
                events: events.clone(),
                started: started.clone(),
            };
            Box::pin(async move { Ok(Box::new(client) as Box<dyn PoolClient>) })
        })
    };
    let pool = BrowserPool::start(factory, options(1)).await.unwrap();
    for (kind, kwargs, expected) in [
        ("success", json!({"ui_delay":0,"min_success":2}), json!(2)),
        ("panic", json!({"ui_delay":[0,0]}), json!(null)),
        (
            "success",
            json!({"ui_delay":null,"min_success":null}),
            json!(null),
        ),
        ("success", json!({}), json!(5)),
    ] {
        let id = pool
            .run(
                kind,
                vec![],
                kwargs.as_object().unwrap().clone(),
                Some(1),
                true,
            )
            .unwrap();
        let results = pool
            .wait(Some(std::slice::from_ref(&id)), Some(5), Some(TIMEOUT))
            .await
            .unwrap();
        if kind != "panic" {
            assert_eq!(results[&id].data["target"], expected);
        } else {
            assert_eq!(results[&id].error_type.as_deref(), Some("Panic"));
        }
    }
    pool.run(
        "cancel",
        vec![],
        json!({"ui_delay":3}).as_object().unwrap().clone(),
        Some(1),
        false,
    )
    .unwrap();
    tokio::time::timeout(TIMEOUT, started.notified())
        .await
        .unwrap();
    pool.shutdown(false).await.unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            json!(0),
            json!([0, 0]),
            json!([1, 2]),
            json!([1, 2]),
            json!(3)
        ]
    );
}

#[tokio::test]
async fn status_counts_held_running_and_business_failure_once() {
    let (events, factory) = fixture();
    let pool = BrowserPool::start(factory, options(1)).await.unwrap();
    let held = pool
        .run("business_false", vec![], Map::new(), None, true)
        .unwrap();
    let active = pool.run("gate", vec![], Map::new(), None, false).unwrap();
    tokio::time::timeout(TIMEOUT, events.started.notified())
        .await
        .unwrap();
    let status = pool.get_status();
    assert_eq!(status["running"], true);
    assert_eq!(status["pending_jobs"], 2);
    assert_eq!(status["queue_size"], 0);
    assert_eq!(status["priority_queue_size"], 0);
    let workers = status["workers"].as_object().unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers.values().next().unwrap()["status"], "busy");
    events.gate.add_permits(1);
    pool.wait(Some(&[held, active]), None, Some(TIMEOUT))
        .await
        .unwrap();
    let status = pool.get_status();
    assert_eq!(status["pending_jobs"], 0);
    assert_eq!(status["completed_jobs"], 2);
    assert_eq!(status["success_count"], 1);
    assert_eq!(status["fail_count"], 1);
    pool.shutdown(true).await.unwrap();
    assert_eq!(pool.get_status()["running"], false);
    assert_eq!(pool.get_status()["workers"], json!({}));
}

#[tokio::test]
async fn wait_timeout_reports_budget_without_cancelling_the_job() {
    let (events, factory) = fixture();
    let pool = BrowserPool::start(factory, options(1)).await.unwrap();
    let id = pool.run("gate", vec![], Map::new(), None, false).unwrap();
    tokio::time::timeout(TIMEOUT, events.started.notified())
        .await
        .unwrap();
    let budget = Duration::from_millis(10);
    for error in [
        pool.wait_for(&id, Some(budget)).await.unwrap_err(),
        pool.wait(Some(std::slice::from_ref(&id)), None, Some(budget))
            .await
            .unwrap_err(),
    ] {
        match error {
            sudohand_browser::Error::Timeout { seconds, .. } => {
                assert_eq!(seconds, budget.as_secs_f64())
            }
            other => panic!("expected timeout, got {other}"),
        }
    }
    let worker = *pool.workers().keys().next().unwrap();
    assert!(!pool.wait_current_task(worker, Some(budget)).await.unwrap());
    assert!(pool.get_result(&id).is_none());
    assert_eq!(pool.pending_count(), 1);
    events.gate.add_permits(1);
    assert!(pool.wait_for(&id, Some(TIMEOUT)).await.unwrap().success);
    assert_eq!(events.calls.lock().unwrap().len(), 1);
    pool.shutdown(true).await.unwrap();
}

#[tokio::test]
async fn interrupted_close_keeps_workers_joinable_for_later_shutdown() {
    struct SlowClose {
        gate: Arc<Semaphore>,
        closed: Arc<Mutex<usize>>,
    }
    impl PoolClient for SlowClose {
        fn execute<'a>(
            &'a mut self,
            _job: &'a Job,
            _context: JobContext,
        ) -> PoolFuture<'a, std::result::Result<ExecutionResult, JobFailure>> {
            Box::pin(async { Ok(json!(null).into()) })
        }

        fn close(&mut self, _close_browser: bool) -> PoolFuture<'_, Result<()>> {
            Box::pin(async move {
                self.gate.acquire().await.unwrap().forget();
                *self.closed.lock().unwrap() += 1;
                Ok(())
            })
        }
    }

    for remove_first in [false, true] {
        let gate = Arc::new(Semaphore::new(0));
        let closed = Arc::new(Mutex::new(0));
        let factory: ClientFactory = {
            let gate = gate.clone();
            let closed = closed.clone();
            Arc::new(move |_| {
                let client = SlowClose {
                    gate: gate.clone(),
                    closed: closed.clone(),
                };
                Box::pin(async move { Ok(Box::new(client) as Box<dyn PoolClient>) })
            })
        };
        let pool = BrowserPool::start(factory, options(2)).await.unwrap();
        let id = *pool.workers().keys().next().unwrap();
        let short = Duration::from_millis(20);
        let initial = async {
            if remove_first {
                pool.remove_worker(id, true).await
            } else {
                pool.shutdown(true).await
            }
        };
        assert!(tokio::time::timeout(short, initial).await.is_err());
        // Cancelling a join must not detach the worker and make the next
        // shutdown falsely report success before browser cleanup finishes.
        assert!(tokio::time::timeout(short, pool.shutdown(false))
            .await
            .is_err());
        assert_eq!(*closed.lock().unwrap(), 0);
        gate.add_permits(2);
        tokio::time::timeout(TIMEOUT, pool.shutdown(false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*closed.lock().unwrap(), 2);
        assert_eq!(pool.worker_count(), 0);
    }
}
