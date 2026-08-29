//! Minimal CDP transport over tokio-tungstenite.
//!
//! Types come from `chromiumoxide_cdp` (generated from the DevTools
//! protocol spec); this module only moves JSON. Deliberately not
//! chromiumoxide's `Browser`/`Handler`: we want a library, not a framework.

pub mod http;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chromiumoxide_types::Command;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::{Error, Result};

/// Per-command timeout.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for input dispatches — a blocked page handler must fail fast.
pub const MOUSE_EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MESSAGE_SIZE: usize = 1 << 28;

/// A CDP event as received off the wire.
#[derive(Debug, Clone)]
pub struct Event {
    /// `Domain.eventName`.
    pub method: String,
    /// Raw params.
    pub params: Value,
    /// Flat-session id, for events from attached targets.
    pub session_id: Option<String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

/// One WebSocket connection to a CDP endpoint (browser or page target).
#[derive(Debug)]
pub struct Connection {
    url: String,
    outbound: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicU64,
    events: broadcast::Sender<Arc<Event>>,
    closed: Arc<AtomicBool>,
}

impl Connection {
    /// Open the WebSocket and start the reader/writer tasks.
    pub async fn connect(url: &str) -> Result<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_SIZE))
            .max_frame_size(Some(MAX_MESSAGE_SIZE));
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), false)
            .await
            .map_err(|e| Error::Connection(format!("websocket connect {url}: {e}")))?;
        let (mut sink, mut stream) = ws.split();
        let (outbound, mut outbound_rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (events, _) = broadcast::channel(1024);
        let closed = Arc::new(AtomicBool::new(false));

        // Writer
        {
            let closed = Arc::clone(&closed);
            tokio::spawn(async move {
                while let Some(text) = outbound_rx.recv().await {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        closed.store(true, Ordering::SeqCst);
                        break;
                    }
                }
                let _ = sink.close().await;
            });
        }
        // Reader
        {
            let pending = Arc::clone(&pending);
            let events = events.clone();
            let closed = Arc::clone(&closed);
            tokio::spawn(async move {
                while let Some(msg) = stream.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t.to_string(),
                        Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                        Ok(Message::Close(_)) | Err(_) => break,
                        Ok(_) => continue,
                    };
                    let Ok(value) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    if let Some(id) = value.get("id").and_then(Value::as_u64) {
                        let tx = pending.lock().ok().and_then(|mut p| p.remove(&id));
                        if let Some(tx) = tx {
                            let outcome = match value.get("error") {
                                Some(err) => Err(Error::Protocol {
                                    method: String::new(),
                                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                                    message: err
                                        .get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or("unknown CDP error")
                                        .to_string(),
                                }),
                                None => Ok(value
                                    .get("result")
                                    .cloned()
                                    .unwrap_or_else(|| Value::Object(serde_json::Map::new()))),
                            };
                            let _ = tx.send(outcome);
                        }
                    } else if let Some(method) = value.get("method").and_then(Value::as_str) {
                        let _ = events.send(Arc::new(Event {
                            method: method.to_string(),
                            params: value.get("params").cloned().unwrap_or(Value::Null),
                            session_id: value
                                .get("sessionId")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        }));
                    }
                }
                closed.store(true, Ordering::SeqCst);
                if let Ok(mut p) = pending.lock() {
                    for (_, tx) in p.drain() {
                        let _ = tx.send(Err(Error::Connection(
                            "WebSocket listener stopped".to_string(),
                        )));
                    }
                }
            });
        }
        Ok(Self {
            url: url.to_string(),
            outbound,
            pending,
            next_id: AtomicU64::new(0),
            events,
            closed,
        })
    }

    /// The endpoint this connection is attached to.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// True once either direction of the socket has failed.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Subscribe to events. Events emitted before subscribing are not replayed.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.events.subscribe()
    }

    /// Send a typed command with the default timeout.
    pub async fn send<C: Command>(&self, cmd: C) -> Result<C::Response> {
        self.send_with(cmd, COMMAND_TIMEOUT, None).await
    }

    /// Send a typed command with an explicit timeout / flat-session id.
    pub async fn send_with<C: Command>(
        &self,
        cmd: C,
        timeout: Duration,
        session_id: Option<&str>,
    ) -> Result<C::Response> {
        let method = cmd.identifier().to_string();
        let params = serde_json::to_value(&cmd)?;
        let raw = self
            .send_raw_with(&method, params, timeout, session_id)
            .await?;
        serde_json::from_value(raw).map_err(Error::Json)
    }

    /// Send `method` with raw JSON `params`; returns the raw `result`.
    pub async fn send_raw(&self, method: &str, params: Value) -> Result<Value> {
        self.send_raw_with(method, params, COMMAND_TIMEOUT, None)
            .await
    }

    /// Raw send with timeout and optional session id.
    pub async fn send_raw_with(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        session_id: Option<&str>,
    ) -> Result<Value> {
        if self.closed() {
            return Err(Error::Connection("WebSocket is closed".to_string()));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, tx);
        }
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(sid) = session_id {
            msg["sessionId"] = Value::String(sid.to_string());
        }
        if self.outbound.send(msg.to_string()).is_err() {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            return Err(Error::Connection("WebSocket send failed".to_string()));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(Error::Protocol { code, message, .. }))) => Err(Error::Protocol {
                method: method.to_string(),
                code,
                message,
            }),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(Error::Connection("connection dropped".to_string())),
            Err(_) => {
                if let Ok(mut p) = self.pending.lock() {
                    p.remove(&id);
                }
                Err(Error::Timeout {
                    method: method.to_string(),
                    seconds: timeout.as_secs_f64(),
                })
            }
        }
    }
}
