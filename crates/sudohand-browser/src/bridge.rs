//! Local WebSocket multiplexer for the browser extension. No Python runtime.
use crate::{cdp::Connection, Error, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
};

/// Shared port convention used by the browser extension.
pub const PORT: u16 = 9522;
type Sender = mpsc::UnboundedSender<Message>;

#[derive(Default)]
struct State {
    next: u64,
    extension: Option<(u64, Sender)>,
    account: Value,
    drivers: HashMap<u64, (Option<String>, Sender)>,
    pending: HashMap<u64, (u64, Value)>,
}

#[allow(clippy::needless_pass_by_value)] // Own the transient JSON at the serialization boundary.
fn emit(sender: &Sender, value: Value) {
    let _ = sender.send(Message::Text(value.to_string().into()));
}

impl State {
    fn remove(&mut self, id: u64) {
        self.drivers.remove(&id);
        self.pending.retain(|_, (driver, _)| *driver != id);
        if self
            .extension
            .as_ref()
            .is_some_and(|(owner, _)| *owner == id)
        {
            self.extension = None;
            self.account = Value::Null;
            for (_, (driver, original)) in self.pending.drain() {
                if let Some((_, sender)) = self.drivers.get(&driver) {
                    emit(
                        sender,
                        json!({"id": original, "error": {"message": "extension disconnected"}}),
                    );
                }
            }
        }
    }

    fn frame(&mut self, id: u64, sender: &Sender, target: Option<&str>, message: &Value) {
        if message.get("_hello").is_some() {
            if self
                .extension
                .as_ref()
                .is_some_and(|(owner, _)| *owner != id)
            {
                let _ = sender.send(Message::Close(None));
                return;
            }
            self.extension = Some((id, sender.clone()));
            self.account = message["account"].clone();
            return;
        }
        if self
            .extension
            .as_ref()
            .is_some_and(|(owner, _)| *owner == id)
        {
            if let Some(gid) = message["_gid"].as_u64() {
                if let Some((driver, original)) = self.pending.remove(&gid) {
                    if let Some((_, sender)) = self.drivers.get(&driver) {
                        let mut reply = json!({"id": original});
                        if let Some(error) = message.get("error") {
                            reply["error"] = error.clone();
                        } else {
                            reply["result"] =
                                message.get("result").cloned().unwrap_or_else(|| json!({}));
                        }
                        emit(sender, reply);
                    }
                }
            } else if let Some(tab) = message["_event_tab"].as_str() {
                for (wanted, sender) in self.drivers.values() {
                    if wanted.as_deref() == Some(tab) {
                        let mut event =
                            json!({"method": message["method"], "params": message["params"]});
                        if let Some(session) = message.get("sessionId") {
                            event["sessionId"] = session.clone();
                        }
                        emit(sender, event);
                    }
                }
            }
            return;
        }
        let original = message["id"].clone();
        if message["method"] == "_bridge.status" {
            emit(
                sender,
                json!({"id": original, "result": {"extension_connected": self.extension.is_some(), "account": self.account, "implementation": "sudohand"}}),
            );
            return;
        }
        let Some((_, extension)) = &self.extension else {
            emit(
                sender,
                json!({"id": original, "error": {"message": "extension not connected"}}),
            );
            return;
        };
        if self.pending.len() >= 4096 {
            emit(
                sender,
                json!({"id": original, "error": {"message": "bridge request limit reached"}}),
            );
            return;
        }
        self.next += 1;
        self.pending.insert(self.next, (id, original));
        self.drivers
            .insert(id, (target.map(str::to_owned), sender.clone()));
        let mut request = json!({"_gid": self.next, "tab": target, "method": message["method"], "params": message.get("params").cloned().unwrap_or_else(|| json!({}))});
        if let Some(session) = message.get("sessionId") {
            request["sessionId"] = session.clone();
        }
        emit(extension, request);
    }
}

#[allow(clippy::result_large_err)] // tungstenite fixes the handshake callback error type.
async fn peer(
    stream: TcpStream,
    state: Arc<Mutex<State>>,
    mut stop: watch::Receiver<bool>,
    shutdown: watch::Sender<bool>,
    id: u64,
) {
    let mut target = None;
    let handshake = accept_hdr_async(stream, |request: &Request, response: Response| {
        // Native clients have no Origin. Websites cannot drive the local bridge.
        if let Some(origin) = request.headers().get("origin") {
            if !origin
                .to_str()
                .is_ok_and(|value| value.starts_with("chrome-extension://"))
            {
                let mut denied = tokio_tungstenite::tungstenite::http::Response::new(Some(
                    "web origins are not allowed".into(),
                ));
                *denied.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
                return Err(denied);
            }
        }
        target = request
            .uri()
            .path()
            .strip_prefix("/devtools/page/")
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        Ok(response)
    });
    let Ok(Ok(socket)) = tokio::time::timeout(Duration::from_secs(5), handshake).await else {
        return;
    };
    let (mut sink, mut stream) = socket.split();
    let (sender, mut output) = mpsc::unbounded_channel();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            Some(message) = output.recv() => {
                if sink.send(message).await.is_err() { break; }
            }
            message = stream.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(value) = serde_json::from_str::<Value>(&text) else { continue; };
                        if value["method"] == "_bridge.shutdown" {
                            let _ = sink.send(Message::Text(json!({"id": value["id"], "result": {"stopped": true}}).to_string().into())).await;
                            let _ = shutdown.send(true);
                            break;
                        }
                        state.lock().expect("bridge state").frame(id, &sender, target.as_deref(), &value);
                    }
                    Some(Ok(Message::Ping(bytes))) => { let _ = sink.send(Message::Pong(bytes)).await; }
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    _ => {}
                }
            }
        }
    }
    state.lock().expect("bridge state").remove(id);
    let _ = sink.close().await;
}

/// Serve until a local native client requests shutdown. Bind loopback only.
pub async fn serve(port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let state = Arc::new(Mutex::new(State::default()));
    let (shutdown, mut stop) = watch::channel(false);
    let mut peers = tokio::task::JoinSet::new();
    let mut id = 0;
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            connection = listener.accept() => {
                let (stream, _) = connection?;
                id += 1;
                peers.spawn(peer(stream, state.clone(), stop.clone(), shutdown.clone(), id));
            }
            Some(_) = peers.join_next(), if !peers.is_empty() => {}
        }
    }
    while peers.join_next().await.is_some() {}
    Ok(())
}

/// Query bridge state without attaching to a page or launching a browser.
pub async fn status(port: u16) -> Option<Value> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let connection = Connection::connect(&format!("ws://127.0.0.1:{port}/devtools/browser"))
            .await
            .ok()?;
        connection.send_raw("_bridge.status", json!({})).await.ok()
    })
    .await
    .ok()
    .flatten()
}

/// Stop only a bridge that identifies itself as sudohand, never an unrelated PID.
pub async fn disconnect(port: u16) -> Result<Value> {
    let Some(status) = status(port).await else {
        return Ok(json!({"stopped": true, "was_running": false}));
    };
    if status["implementation"] != "sudohand" {
        return Err(Error::Invalid("This bridge was started by another implementation; stop it using that implementation's browser_disconnect".into()));
    }
    let connection =
        Connection::connect(&format!("ws://127.0.0.1:{port}/devtools/browser")).await?;
    connection.send_raw("_bridge.shutdown", json!({})).await?;
    Ok(json!({"stopped": true, "was_running": true}))
}
