//! Transport tests use a synthetic extension; real Chrome tests are separate.
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use sudohand_browser::bridge;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn read(socket: &mut Socket) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}
async fn send(socket: &mut Socket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn routes_colliding_ids_events_disconnect_and_shutdown() {
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let server = tokio::spawn(bridge::serve(port));
    tokio::time::timeout(Duration::from_secs(5), async {
        while bridge::status(port).await.is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let base = format!("ws://127.0.0.1:{port}");
    let mut forbidden = base.clone().into_client_request().unwrap();
    forbidden
        .headers_mut()
        .insert("Origin", "https://website.test".parse().unwrap());
    assert!(connect_async(forbidden).await.is_err());
    let (mut extension, _) = connect_async(&base).await.unwrap();
    send(
        &mut extension,
        json!({"_hello": true, "account": "fixture@example.test"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while bridge::status(port).await.unwrap()["account"] != "fixture@example.test" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (mut first, _) = connect_async(format!("{base}/devtools/page/11"))
        .await
        .unwrap();
    let (mut second, _) = connect_async(format!("{base}/devtools/page/22"))
        .await
        .unwrap();
    send(
        &mut first,
        json!({"id": 7, "method": "Runtime.evaluate", "params": {"expression": "1"}}),
    )
    .await;
    let one = read(&mut extension).await;
    send(
        &mut second,
        json!({"id": 7, "method": "Runtime.evaluate", "params": {"expression": "2"}}),
    )
    .await;
    let two = read(&mut extension).await;
    assert_eq!(one["tab"], "11");
    assert_eq!(two["tab"], "22");
    assert_ne!(one["_gid"], two["_gid"]);
    send(
        &mut extension,
        json!({"_gid": two["_gid"], "result": {"value": 2}}),
    )
    .await;
    send(
        &mut extension,
        json!({"_gid": one["_gid"], "result": {"value": 1}}),
    )
    .await;
    assert_eq!(
        read(&mut first).await,
        json!({"id": 7, "result": {"value": 1}})
    );
    assert_eq!(
        read(&mut second).await,
        json!({"id": 7, "result": {"value": 2}})
    );
    send(
        &mut extension,
        json!({"_event_tab": "22", "method": "Page.loadEventFired", "params": {"timestamp": 1}}),
    )
    .await;
    assert_eq!(read(&mut second).await["method"], "Page.loadEventFired");
    send(&mut first, json!({"id": 8, "method": "DOM.getDocument"})).await;
    read(&mut extension).await;
    extension.close(None).await.unwrap();
    assert_eq!(
        read(&mut first).await,
        json!({"id": 8, "error": {"message": "extension disconnected"}})
    );
    assert_eq!(
        bridge::disconnect(port).await.unwrap(),
        json!({"stopped": true, "was_running": true})
    );
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        bridge::disconnect(port).await.unwrap()["was_running"],
        false
    );
}
