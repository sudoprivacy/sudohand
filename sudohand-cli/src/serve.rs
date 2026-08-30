//! `suh serve` — expose the actuators over a WebSocket.
//!
//! This is the bridge, living inside sudohand itself: `suh serve` listens on a
//! WS and every agent — a remote apeiron pod *and* a local sudocode — connects
//! to it as a client. One path, one protocol, wherever the caller sits. suh
//! still executes on this machine (the one with the apps); the WS only carries
//! the call in and the result out.
//!
//! ## Protocol (v1) — JSON text frames
//!
//! On connect (after auth, if a `--token` is set) the server sends the
//! capability catalog:
//! ```json
//! {"type":"ready","server":"suh","version":"…","methods":[
//!    {"method":"wx.context","description":"…","readOnly":true},
//!    {"method":"desktop.click","description":"…","readOnly":false}, … ]}
//! ```
//! The client calls one action per frame:
//! ```json
//! {"type":"call","id":"1","method":"desktop.click","params":{"x":448,"y":725}}
//! ```
//! `params` are named flags (bool → present/absent, array → repeated); an
//! optional `args` array carries positionals. The reply echoes `id` and
//! preserves the CLI's exit-code contract:
//! ```json
//! {"type":"result","id":"1","ok":true,"result":{…},"exit":0}
//! {"type":"result","id":"1","ok":false,"error":{"kind":"…","message":"…"},"exit":4}
//! ```
//!
//! A call is dispatched by re-invoking `suh` (this same binary) with an argv
//! built from the frame, so extensions (`suh wx …`) and workflows are reachable
//! the moment they're installed, with no per-method code here. `shell` and the
//! meta commands are refused — the WS is not a remote shell.

use clap::CommandFactory;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// Methods never reachable over the wire: arbitrary shell, extension
/// management, and the meta/self commands. Actuators and extensions are
/// allowed by default (deny-list, not allow-list, so a new `suh wx` verb is
/// callable without touching this file).
const DENY_DOMAINS: &[&str] = &["shell", "ext", "describe", "serve", "mcp", "help"];

/// Start the server and block until it stops. Builds its own multi-thread
/// runtime so `main` can stay synchronous like the rest of the CLI.
pub fn run(bind: String, token: Option<String>) -> std::process::ExitCode {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{}", err_envelope("internal", &format!("runtime: {e}")));
            return std::process::ExitCode::from(1);
        }
    };
    match rt.block_on(serve(&bind, token)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", err_envelope("io", &e));
            std::process::ExitCode::from(9)
        }
    }
}

async fn serve(bind: &str, token: Option<String>) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    eprintln!(
        "suh serve: listening on ws://{bind} (auth: {})",
        if token.is_some() { "token" } else { "none" }
    );
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                eprintln!("suh serve: accept error: {e}");
                continue;
            }
        };
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, token).await {
                eprintln!("suh serve: connection {peer} closed: {e}");
            }
        });
    }
}

async fn handle_conn(stream: tokio::net::TcpStream, token: Option<String>) -> Result<(), String> {
    let ws = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| format!("ws handshake: {e}"))?;
    let (mut write, mut read) = ws.split();

    // Optional auth: the first frame must be {"type":"auth","token":"…"}.
    if let Some(expected) = &token {
        let ok = match read.next().await {
            Some(Ok(Message::Text(t))) => {
                serde_json::from_str::<Value>(&t)
                    .ok()
                    .filter(|v| v.get("type").and_then(Value::as_str) == Some("auth"))
                    .and_then(|v| v.get("token").and_then(Value::as_str).map(str::to_owned))
                    .as_deref()
                    == Some(expected.as_str())
            }
            _ => false,
        };
        if !ok {
            let _ = write
                .send(Message::Text(
                    json!({"type":"error","message":"unauthorized"})
                        .to_string()
                        .into(),
                ))
                .await;
            return Ok(());
        }
    }

    // Advertise the catalog.
    write
        .send(Message::Text(ready_frame().to_string().into()))
        .await
        .map_err(|e| format!("send ready: {e}"))?;

    // One call per frame; replies preserve `id`.
    while let Some(msg) = read.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t.to_string(),
            Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        let reply = handle_call(&text).await;
        if write
            .send(Message::Text(reply.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
    Ok(())
}

/// The capability catalog: every built-in `domain.action` with its one-line
/// description and read-only flag. (Extensions aren't enumerated yet — they
/// don't self-describe — but remain callable.)
fn ready_frame() -> Value {
    let root = crate::Cli::command();
    let mut methods = Vec::new();
    for domain in root.get_subcommands() {
        let dname = domain.get_name();
        if DENY_DOMAINS.contains(&dname) {
            continue;
        }
        for action in domain.get_subcommands() {
            let aname = action.get_name();
            if aname == "help" {
                continue;
            }
            methods.push(json!({
                "method": format!("{dname}.{aname}"),
                "description": action.get_about().map(|s| one_line(&s.to_string())).unwrap_or_default(),
                "readOnly": is_read_only(aname),
            }));
        }
    }
    json!({
        "type": "ready",
        "server": "suh",
        "version": env!("CARGO_PKG_VERSION"),
        "methods": methods,
    })
}

/// Handle one `call` frame → a `result` frame. Validates the method, builds
/// the argv, re-invokes `suh`, and maps stdout/stderr/exit onto the reply.
async fn handle_call(text: &str) -> Value {
    let msg: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return json!({"type":"result","ok":false,"error":{"kind":"invalid_input","message":format!("bad json: {e}")}})
        }
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let fail = |kind: &str, m: String| json!({"type":"result","id":id,"ok":false,"error":{"kind":kind,"message":m}});

    if msg.get("type").and_then(Value::as_str) != Some("call") {
        return fail("invalid_input", "expected {\"type\":\"call\"}".into());
    }
    let method = match msg.get("method").and_then(Value::as_str) {
        Some(m) => m,
        None => return fail("invalid_input", "missing method".into()),
    };
    let Some((domain, action)) = method.split_once('.') else {
        return fail(
            "invalid_input",
            format!("method must be <domain>.<action>, got {method:?}"),
        );
    };
    if DENY_DOMAINS.contains(&domain) || domain.is_empty() || action.is_empty() {
        return fail(
            "permission_denied",
            format!("method {method:?} is not exposed"),
        );
    }

    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let args = msg.get("args").cloned().unwrap_or(json!([]));
    let argv = build_argv(domain, action, &params, &args);

    let exe = std::env::var_os("SUH_BIN")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| "suh".into());

    match tokio::process::Command::new(&exe)
        .args(&argv)
        .output()
        .await
    {
        Ok(o) => {
            let exit = o.status.code().unwrap_or(-1);
            if o.status.success() {
                // stdout is JSON (or TSV): pass JSON through, wrap TSV as text.
                let result = serde_json::from_slice::<Value>(&o.stdout)
                    .unwrap_or_else(|_| json!({"text": String::from_utf8_lossy(&o.stdout)}));
                json!({"type":"result","id":id,"ok":true,"result":result,"exit":exit})
            } else {
                // Prefer the CLI's own error envelope from stderr.
                let error = serde_json::from_slice::<Value>(&o.stderr)
                    .ok()
                    .and_then(|v| v.get("error").cloned())
                    .unwrap_or_else(|| json!({"kind":"internal","message": String::from_utf8_lossy(&o.stderr).trim()}));
                json!({"type":"result","id":id,"ok":false,"error":error,"exit":exit})
            }
        }
        Err(e) => fail("io", format!("failed to run suh: {e}")),
    }
}

/// `[domain, action, <positionals…>, --flag value …]` from a call frame.
/// Bool `true` → bare `--flag`; `false` → omitted; array → the flag repeated.
/// Everything the CLI itself re-validates, so unknown keys are harmless.
fn build_argv(domain: &str, action: &str, params: &Value, args: &Value) -> Vec<String> {
    let mut argv = vec![domain.to_string(), action.to_string()];
    if let Some(arr) = args.as_array() {
        for a in arr {
            argv.push(scalar(a));
        }
    }
    if let Some(obj) = params.as_object() {
        for (k, v) in obj {
            match v {
                Value::Bool(true) => argv.push(format!("--{k}")),
                Value::Bool(false) => {}
                Value::Array(items) => {
                    for e in items {
                        argv.push(format!("--{k}"));
                        argv.push(scalar(e));
                    }
                }
                _ => {
                    argv.push(format!("--{k}"));
                    argv.push(scalar(v));
                }
            }
        }
    }
    argv
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Conservative read-only classifier (same policy the access design uses):
/// an action acts unless it's a known observe-only verb — a mutating method is
/// never mislabeled read-only. The authoritative class should move into
/// `suh describe`; this allowlist is the interim source.
fn is_read_only(action: &str) -> bool {
    const READ: &[&str] = &[
        "status",
        "apps",
        "ax-tree",
        "ax-find",
        "find-window",
        "locate",
        "ask",
        "screenshot",
        "read",
        "ls",
        "stat",
        "exists",
        "browser_list",
        "page_info",
        "page_html",
        "page_screenshot",
        "page_discover",
        "page_pdf",
        "page_wait_url",
        "page_wait_element",
        "page_wait_ready",
        "tab_list",
        "storage_get",
        "cookies_list",
        "find_by_text",
        "find_by_html_id",
        "find_by_xpath",
        "html_by_ref",
        "screenshot_by_ref",
    ];
    READ.contains(&action)
}

fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or("").replace('\t', " ")
}

fn err_envelope(kind: &str, message: &str) -> String {
    json!({"error": {"kind": kind, "message": message}}).to_string()
}
