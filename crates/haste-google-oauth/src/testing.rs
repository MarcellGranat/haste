//! A tiny local HTTP server for tests: records requests and answers from a handler.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: String,
}

pub struct FakeServer {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
}

impl FakeServer {
    #[must_use]
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }

    #[must_use]
    pub fn count(&self, path_prefix: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.path.starts_with(path_prefix))
            .count()
    }
}

/// Start a server on `127.0.0.1:0`. The handler returns `(status, json body)`.
///
/// # Panics
/// If the loopback listener cannot be bound.
pub async fn spawn<F>(handler: F) -> FakeServer
where
    F: Fn(&Recorded) -> (u16, String) + Send + Sync + 'static,
{
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let log = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let handler = Arc::clone(&handler);
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0_u8; 4096];
                let header_end = loop {
                    let Ok(n) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                let content_length = head
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while buf.len() < header_end + content_length {
                    let Ok(n) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body = String::from_utf8_lossy(&buf[header_end..]).into_owned();
                let mut first = head.lines().next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("").to_owned();
                let target = first.next().unwrap_or("/");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let recorded = Recorded {
                    method,
                    path: path.to_owned(),
                    query: query.to_owned(),
                    body,
                };
                if let Ok(mut l) = log.lock() {
                    l.push(recorded.clone());
                }
                let (status, json) = handler(&recorded);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    FakeServer { base, requests }
}
