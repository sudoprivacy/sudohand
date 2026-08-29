//! Tiny HTTP/1.1 GET for Chrome's `/json/*` endpoints — enough to avoid a
//! full HTTP client dependency.

use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{Error, Result};

/// `GET http://{host}:{port}{path}` and parse the JSON body.
///
/// Chrome's endpoint ignores `Connection: close`, so we must stop at
/// `Content-Length` instead of reading to EOF.
pub async fn get_json(host: &str, port: u16, path: &str, timeout: Duration) -> Result<Value> {
    let fut = async {
        let mut stream = TcpStream::connect((host, port)).await?;
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await?;
        let mut buf: Vec<u8> = Vec::with_capacity(4096);
        let mut chunk = [0u8; 4096];
        // Headers first.
        let header_end = loop {
            if let Some(pos) = find_header_end(&buf) {
                break pos;
            }
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::Error::other("EOF before end of HTTP headers"));
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let body_start = header_end + 4;
        let content_length =
            header_value(&head, "content-length").and_then(|v| v.parse::<usize>().ok());
        let chunked = header_value(&head, "transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
        match content_length {
            Some(len) => {
                while buf.len() < body_start + len {
                    let n = stream.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
            None => {
                // Chunked or until EOF.
                loop {
                    if chunked && buf[body_start..].windows(5).any(|w| w == b"0\r\n\r\n") {
                        break;
                    }
                    let n = stream.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
        let body = String::from_utf8_lossy(&buf[body_start.min(buf.len())..]).into_owned();
        Ok::<(String, String, bool), std::io::Error>((head, body, chunked))
    };
    let (head, body, chunked) = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| Error::Connection(format!("HTTP GET {host}:{port}{path} timed out")))?
        .map_err(|e| Error::Connection(format!("HTTP GET {host}:{port}{path}: {e}")))?;
    let status_ok = head
        .lines()
        .next()
        .is_some_and(|l| l.split_whitespace().nth(1) == Some("200"));
    if !status_ok {
        return Err(Error::Connection(format!(
            "HTTP GET {path}: {}",
            head.lines().next().unwrap_or("no status")
        )));
    }
    let body = if chunked { dechunk(&body) } else { body };
    serde_json::from_str(body.trim()).map_err(Error::Json)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Case-insensitive header lookup; tolerates `Name:value` without a space
/// (which is how Chrome writes `Content-Length`).
fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size_line.trim(), 16) else {
            break;
        };
        if size == 0 {
            break;
        }
        let chunk: String = after.chars().take(size).collect();
        out.push_str(&chunk);
        rest = &after[chunk.len().min(after.len())..];
        rest = rest.strip_prefix("\r\n").unwrap_or(rest);
    }
    out
}

/// `webSocketDebuggerUrl` from `/json/version`.
pub async fn ws_debugger_url(host: &str, port: u16, timeout: Duration) -> Result<String> {
    let info = get_json(host, port, "/json/version", timeout).await?;
    info.get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Connection("no webSocketDebuggerUrl in /json/version".to_string()))
}
