//! Shared test scaffolding: a fixture HTTP server (no external network) and
//! a per-test headless Chrome that is killed on drop.

#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc
)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use sudohand_browser::browser::{browser_start, StartOptions};
use sudohand_browser::chrome::Headless;
use sudohand_browser::connection::{get_active_tab, BrowserClient, Tab};

/// Serves `tests/fixtures/` over `http://127.0.0.1:<port>/`.
pub struct Fixtures {
    pub base_url: String,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn handle(mut stream: TcpStream, root: &Path) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");
    let path = path
        .split('?')
        .next()
        .unwrap_or("/")
        .trim_start_matches('/');
    let file = root.join(if path.is_empty() { "index.html" } else { path });
    let (status, body) = match std::fs::read(&file) {
        Ok(b) => ("200 OK", b),
        Err(_) => ("404 Not Found", b"not found".to_vec()),
    };
    let ctype = if file.extension().is_some_and(|e| e == "html") {
        "text/html; charset=utf-8"
    } else {
        "application/octet-stream"
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

impl Fixtures {
    pub fn serve() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let port = listener.local_addr().unwrap().port();
        let root = fixtures_dir();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let root = root.clone();
                std::thread::spawn(move || handle(stream, &root));
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
        }
    }

    pub fn url(&self, name: &str) -> String {
        format!("{}/{name}", self.base_url)
    }
}

/// A headless Chrome owned by one test. Killed (process group) on drop.
pub struct Chrome {
    pub port: u16,
    pub pid: u32,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = sudohand_browser::port::kill_process_tree(self.pid);
        let _ = sudohand_browser::port::cleanup_temp_profile(self.port);
    }
}

fn test_browser_port() -> u16 {
    // Windows CI observed a bind collision after releasing a bind(:0) probe.
    // Such a port can become an outgoing client's source port. Stay below the
    // usual ephemeral ranges, and allocate distinct candidates across sibling
    // tests before dropping the bind probe. Occupied ports are still skipped.
    static NEXT_PORT: AtomicUsize = AtomicUsize::new(0);
    for _ in 0..10_000 {
        let port = 20_000 + (NEXT_PORT.fetch_add(1, Ordering::Relaxed) % 10_000) as u16;
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no browser fixture port available in 20000-29999");
}

pub async fn start_chrome() -> Chrome {
    start_chrome_after_probe(test_browser_port()).await
}

/// A bind probe cannot reserve a socket for another process. If a competing
/// listener wins before browser_start checks it, choose a new fixture port.
/// Only this pre-launch validation error is recoverable: never replay a launch
/// after a process has started, or retry any page operation.
pub async fn start_chrome_after_probe(mut port: u16) -> Chrome {
    for attempt in 0..10 {
        match try_start_with_port(Some(port)).await {
            Ok(chrome) => return chrome,
            Err(sudohand_browser::Error::Invalid(message))
                if message.starts_with(&format!("Port {port} is already in use")) =>
            {
                eprintln!(
                    "fixture port {port} taken before launch (attempt {}); allocating another",
                    attempt + 1
                );
                port = test_browser_port();
            }
            Err(error) => panic!("browser_start: {error:?}"),
        }
    }
    panic!("fixture port allocation lost ten consecutive pre-launch races");
}

/// A Chrome on the preferred 9350-9450 band, where `browser_list` scans.
/// The port is chosen explicitly (top of the band) rather than left to
/// `browser_start`, which honours `AI_DEV_BROWSER_PORT` — process env is
/// shared across test threads and another test sets that variable.
pub async fn start_chrome_in_band() -> Chrome {
    let port = sudohand_browser::port::get_available_port((9440, 9450), &[]).expect("in-band port");
    start_with_port(Some(port)).await
}

async fn start_with_port(port: Option<u16>) -> Chrome {
    try_start_with_port(port).await.expect("browser_start")
}

async fn try_start_with_port(port: Option<u16>) -> sudohand_browser::Result<Chrome> {
    // CI's setup-chrome Chromium has no usable SUID sandbox helper; the
    // workflow passes `--no-sandbox` through this test-only hook.
    let extra_args: Vec<String> = std::env::var("ADB_TEST_CHROME_ARGS")
        .map(|v| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let opts = StartOptions {
        port,
        extra_args,
        headless: Some(Headless::New),
        startup_timeout: Some(60.0),
        ..StartOptions::default()
    };
    let r = browser_start(&opts).await?;
    if let Some(err) = r.get("error") {
        panic!(
            "browser_start failed: {err}. Is Chrome installed? Set AI_DEV_BROWSER_CHROME to the executable."
        );
    }
    Ok(Chrome {
        port: r["port"].as_u64().unwrap() as u16,
        pid: r["pid"].as_u64().unwrap() as u32,
    })
}

pub async fn open(chrome: &Chrome, url: &str) -> (BrowserClient, Tab) {
    let mut browser = BrowserClient::connect("127.0.0.1", chrome.port)
        .await
        .expect("connect");
    let tab = get_active_tab(&mut browser, None).await.expect("tab");
    sudohand_browser::tools::page_goto(&tab, url, true)
        .await
        .expect("goto");
    (browser, tab)
}

pub fn skip_browser_tests() -> bool {
    if std::env::var("ADB_SKIP_BROWSER_TESTS").is_ok_and(|v| v == "1") {
        eprintln!("ADB_SKIP_BROWSER_TESTS=1 — skipping");
        return true;
    }
    false
}
