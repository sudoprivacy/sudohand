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

fn ephemeral_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub async fn start_chrome() -> Chrome {
    start_with_port(Some(ephemeral_port())).await
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
    let r = browser_start(&opts).await.expect("browser_start");
    if let Some(err) = r.get("error") {
        panic!(
            "browser_start failed: {err}. Is Chrome installed? Set AI_DEV_BROWSER_CHROME to the executable."
        );
    }
    Chrome {
        port: r["port"].as_u64().unwrap() as u16,
        pid: r["pid"].as_u64().unwrap() as u32,
    }
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
