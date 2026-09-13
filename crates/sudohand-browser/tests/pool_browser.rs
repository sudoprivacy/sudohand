//! A real custom browser client exercising the public pool factory interface.
mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use common::{Chrome, Fixtures};
use serde_json::{json, Map};
use sudohand_browser::{
    browser::{browser_start, StartOptions},
    chrome::Headless,
    connection::{get_active_tab, BrowserClient, Tab},
    pool::*,
    tools, Result,
};

struct BrowserWorker {
    chrome: Option<Chrome>,
    _browser: BrowserClient,
    tab: Tab,
    options: WorkerOptions,
    barrier: Arc<tokio::sync::Barrier>,
}

impl PoolClient for BrowserWorker {
    fn execute<'a>(
        &'a mut self,
        job: &'a Job,
        _context: JobContext,
    ) -> PoolFuture<'a, std::result::Result<ExecutionResult, JobFailure>> {
        Box::pin(async move {
            // Both workers must be live; a sequential scheduler would time out.
            self.barrier.wait().await;
            let result: Result<serde_json::Value> = async {
                tools::page_goto(&self.tab, job.args[0].as_str().unwrap(), true).await?;
                if job.task_type == "restore" {
                    return tools::cookies_extract_live(&self.tab, "parity.test").await;
                }
                let marker = format!("worker-{}", self.options.worker_id);
                self.tab.evaluate(&format!("window.workerMarker = {}", json!(marker))).await?;
                tools::cdp_send(&self.tab, "Storage.setCookies", Some(&json!({"cookies": [{"name":"pool_fixture", "value":marker,"domain":".parity.test","path":"/"}]}).to_string())).await?;
                let cookies = tools::cookies_extract_live(&self.tab, "parity.test").await?;
                Ok(json!({"worker_id":self.options.worker_id, "marker": self.tab.evaluate("window.workerMarker").await?, "cookies":cookies}))
            }.await;
            result
                .map(ExecutionResult::from)
                .map_err(|error| JobFailure::new(error.to_string(), "BrowserError"))
        })
    }

    fn close(&mut self, close_browser: bool) -> PoolFuture<'_, Result<()>> {
        Box::pin(async move {
            assert!(
                close_browser,
                "this fixture owns disposable Chrome instances"
            );
            tools::cookies_save(&self.tab, self.options.cookies_file.as_deref(), None).await?;
            // Dropping the process guard signals Chrome; on Linux the socket
            // can outlive that signal. close() promises completed cleanup.
            drop(self.chrome.take());
            tokio::time::timeout(Duration::from_secs(5), async {
                while sudohand_browser::port::is_port_in_use(self.options.port) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .map_err(|_| {
                sudohand_browser::Error::Invalid(format!(
                    "worker Chrome port {} stayed open after shutdown",
                    self.options.port
                ))
            })?;
            Ok(())
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_chromes_persist_per_worker_cookies_and_close() {
    if common::skip_browser_tests() {
        return;
    }
    let fixtures = Fixtures::serve();
    let directory = tempfile::tempdir().unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let started_ports = Arc::new(Mutex::new(Vec::new()));
    let captured_ports = started_ports.clone();
    let factory: ClientFactory = Arc::new(move |options| {
        let barrier = barrier.clone();
        let ports = captured_ports.clone();
        Box::pin(async move {
            let started = browser_start(&StartOptions {
                port: Some(options.port),
                headless: Some(Headless::New),
                silent_stderr: true,
                extra_args: std::env::var("ADB_TEST_CHROME_ARGS")
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect(),
                ..StartOptions::default()
            })
            .await?;
            assert!(!started["reused"].as_bool().unwrap_or(false));
            let chrome = Chrome {
                port: options.port,
                pid: started["pid"].as_u64().unwrap() as u32,
            };
            ports.lock().unwrap().push(options.port);
            let mut browser = BrowserClient::connect("127.0.0.1", options.port).await?;
            let tab = get_active_tab(&mut browser, None).await?;
            if options
                .cookies_file
                .as_ref()
                .is_some_and(|path| path.exists())
            {
                tools::cookies_load(&tab, options.cookies_file.as_deref()).await?;
            }
            Ok(Box::new(BrowserWorker {
                chrome: Some(chrome),
                _browser: browser,
                tab,
                options,
                barrier,
            }) as Box<dyn PoolClient>)
        })
    });
    let pool = BrowserPool::start(
        factory.clone(),
        PoolOptions {
            workers: 2,
            max_retries: 1,
            headless: true,
            profile: ProfileMode::PerWorker,
            cookies_dir: Some(directory.path().to_path_buf()),
            ..PoolOptions::default()
        },
    )
    .await
    .unwrap();
    let ids = pool
        .submit_batch(
            (0..2)
                .map(|_| {
                    (
                        "visit".into(),
                        vec![json!(fixtures.url("form.html"))],
                        Map::new(),
                    )
                })
                .collect(),
        )
        .unwrap();
    let results = pool
        .wait(Some(&ids), None, Some(Duration::from_secs(20)))
        .await;
    pool.shutdown(false).await.unwrap();
    let results = results.unwrap();
    let mut workers = Vec::new();
    for result in results.values() {
        assert!(result.success, "{result:?}");
        let id = result.worker_id.unwrap();
        workers.push(id);
        assert_eq!(result.data["marker"], format!("worker-{id}"));
        assert_eq!(result.data["cookies"][0]["value"], format!("worker-{id}"));
        let file = directory.path().join(format!("cookies_worker_{id}.dat"));
        let cookies: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        assert!(cookies
            .as_array()
            .unwrap()
            .iter()
            .any(|cookie| cookie["name"] == "pool_fixture"
                && cookie["value"] == format!("worker-{id}")));
    }
    workers.sort_unstable();
    assert_eq!(workers, [0, 1]);
    let restored = BrowserPool::start(
        factory,
        PoolOptions {
            workers: 2,
            max_retries: 1,
            headless: true,
            profile: ProfileMode::PerWorker,
            cookies_dir: Some(directory.path().to_path_buf()),
            ..PoolOptions::default()
        },
    )
    .await
    .unwrap();
    let ids = restored
        .submit_batch(
            (0..2)
                .map(|_| {
                    (
                        "restore".into(),
                        vec![json!(fixtures.url("form.html"))],
                        Map::new(),
                    )
                })
                .collect(),
        )
        .unwrap();
    let outcomes = restored
        .wait(Some(&ids), None, Some(Duration::from_secs(20)))
        .await;
    restored.shutdown(false).await.unwrap();
    for outcome in outcomes.unwrap().values() {
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(
            outcome.data[0]["value"],
            format!("worker-{}", outcome.worker_id.unwrap())
        );
    }
    for port in started_ports.lock().unwrap().iter() {
        assert!(!sudohand_browser::port::is_port_in_use(*port));
    }
}
