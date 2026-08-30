//! `suh serve` — the WebSocket bridge. Boots the real binary, connects a WS
//! client, and checks the v1 protocol end to end: the `ready` catalog, a read
//! call that succeeds, and a denied (`shell`) method. This is also the
//! server's smoke test.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// An OS-assigned free port on loopback (the listener is dropped so `suh
/// serve` can claim it; the brief gap is covered by the connect retry).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A child `suh serve` that is killed when dropped.
struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn text(msg: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>) -> Value {
    match msg {
        Some(Ok(Message::Text(t))) => serde_json::from_str(&t).expect("json frame"),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

#[tokio::test]
async fn ready_catalog_call_and_denial() {
    let port = free_port();
    let server = Server(
        std::process::Command::new(env!("CARGO_BIN_EXE_suh"))
            .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn suh serve"),
    );

    // The listener needs a moment; retry the connect.
    let url = format!("ws://127.0.0.1:{port}");
    let mut ws = None;
    for _ in 0..50 {
        if let Ok((s, _)) = tokio_tungstenite::connect_async(&url).await {
            ws = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    let mut ws = ws.expect("connect to suh serve");

    // 1. The first frame is the capability catalog.
    let ready = text(ws.next().await).await;
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["server"], "suh");
    let methods = ready["methods"].as_array().expect("methods array");
    assert!(methods.len() > 20, "catalog is generated from the tree");
    let status = methods
        .iter()
        .find(|m| m["method"] == "desktop.status")
        .expect("desktop.status advertised");
    assert_eq!(status["readOnly"], true);
    // shell must not be advertised.
    assert!(methods.iter().all(|m| !m["method"].as_str().unwrap().starts_with("shell.")));

    // 2. A read call succeeds and preserves the exit code.
    ws.send(Message::Text(
        json!({"type":"call","id":"a","method":"fs.exists","params":{"path":"/"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let res = text(ws.next().await).await;
    assert_eq!(res["type"], "result");
    assert_eq!(res["id"], "a");
    assert_eq!(res["ok"], true);
    assert_eq!(res["exit"], 0);
    assert_eq!(res["result"]["exists"], true);

    // 3. A denied method (shell) is refused, not executed.
    ws.send(Message::Text(
        json!({"type":"call","id":"b","method":"shell.run","args":["echo","hi"]})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let res = text(ws.next().await).await;
    assert_eq!(res["id"], "b");
    assert_eq!(res["ok"], false);
    assert_eq!(res["error"]["kind"], "permission_denied");

    drop(server);
}

#[tokio::test]
async fn token_auth_rejects_wrong_token() {
    let port = free_port();
    let _server = Server(
        std::process::Command::new(env!("CARGO_BIN_EXE_suh"))
            .args(["serve", "--bind", &format!("127.0.0.1:{port}"), "--token", "s3cret"])
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn suh serve"),
    );

    let url = format!("ws://127.0.0.1:{port}");
    let mut ws = None;
    for _ in 0..50 {
        if let Ok((s, _)) = tokio_tungstenite::connect_async(&url).await {
            ws = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    let mut ws = ws.expect("connect");

    // Wrong token → an error frame, no catalog.
    ws.send(Message::Text(
        json!({"type":"auth","token":"wrong"}).to_string().into(),
    ))
    .await
    .unwrap();
    let frame = text(ws.next().await).await;
    assert_eq!(frame["type"], "error");
    assert_eq!(frame["message"], "unauthorized");
}
