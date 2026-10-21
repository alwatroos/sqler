// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A small HTTP/1.1 mock server on loopback, built on tokio only.
//!
//! It records `(method, path, headers, body)` of every request and answers per route.
//! Every connection is closed after one reply.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};

/// How a chunked reply ends.
#[derive(Clone, Copy)]
pub enum Finish {
    /// Send the `0\r\n\r\n` terminator and close.
    Clean,
    /// Close the socket without the terminator.
    Disconnect,
    /// Keep the socket open and fire `client_closed` when the client hangs up.
    Hold,
}

#[derive(Clone)]
pub enum Body {
    Full(Vec<u8>),
    Chunked { chunks: Vec<Vec<u8>>, end: Finish },
}

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Reply {
    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Body::Full(body.as_bytes().to_vec()),
        }
    }

    /// A server-sent-events reply cut into HTTP chunks of `size` bytes.
    pub fn sse(data: &str, size: usize, end: Finish) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "text/event-stream".into())],
            body: Body::Chunked {
                chunks: data.as_bytes().chunks(size).map(<[u8]>::to_vec).collect(),
                end,
            },
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

#[derive(Clone)]
pub struct Route {
    pub method: &'static str,
    pub path: &'static str,
    /// The route matches only when the request body contains this text.
    pub body_contains: Option<String>,
    pub reply: Reply,
}

impl Route {
    pub fn new(method: &'static str, path: &'static str, reply: Reply) -> Self {
        Self {
            method,
            path,
            body_contains: None,
            reply,
        }
    }

    pub fn when_body_has(mut self, text: &str) -> Self {
        self.body_contains = Some(text.to_owned());
        self
    }
}

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    /// The value of the first header with this name (ASCII case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

pub struct MockServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
    pub client_closed: Arc<Notify>,
    task: JoinHandle<()>,
}

impl MockServer {
    pub async fn start(routes: Vec<Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client_closed = Arc::new(Notify::new());
        let routes = Arc::new(routes);
        let task = tokio::spawn({
            let requests = Arc::clone(&requests);
            let client_closed = Arc::clone(&client_closed);
            async move {
                // Dropping the set (when this task is aborted) aborts the connections.
                let mut conns = JoinSet::new();
                loop {
                    let Ok((sock, _)) = listener.accept().await else {
                        break;
                    };
                    conns.spawn(serve(
                        sock,
                        Arc::clone(&routes),
                        Arc::clone(&requests),
                        Arc::clone(&client_closed),
                    ));
                }
            }
        });
        Self {
            addr,
            requests,
            client_closed,
            task,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A loopback port nobody listens on.
pub async fn dead_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

async fn read_request(sock: &mut TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(length);
    Some(Recorded {
        method,
        path,
        headers,
        body,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Error",
    }
}

async fn serve(
    mut sock: TcpStream,
    routes: Arc<Vec<Route>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    client_closed: Arc<Notify>,
) {
    let _ = sock.set_nodelay(true);
    let Some(req) = read_request(&mut sock).await else {
        return;
    };
    let body_text = String::from_utf8_lossy(&req.body).into_owned();
    let reply = routes
        .iter()
        .find(|r| {
            r.method == req.method
                && r.path == req.path
                && r.body_contains
                    .as_ref()
                    .is_none_or(|t| body_text.contains(t))
        })
        .map_or_else(
            || Reply::json(404, r#"{"error":{"message":"no route"}}"#),
            |r| r.reply.clone(),
        );
    requests.lock().unwrap().push(req);

    let mut head = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\n",
        reply.status,
        reason(reply.status)
    );
    for (k, v) in &reply.headers {
        write!(head, "{k}: {v}\r\n").unwrap();
    }
    match reply.body {
        Body::Full(bytes) => {
            write!(head, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&bytes).await;
            let _ = sock.shutdown().await;
        }
        Body::Chunked { chunks, end } => {
            head.push_str("Transfer-Encoding: chunked\r\n\r\n");
            let _ = sock.write_all(head.as_bytes()).await;
            for c in chunks {
                let mut frame = format!("{:x}\r\n", c.len()).into_bytes();
                frame.extend_from_slice(&c);
                frame.extend_from_slice(b"\r\n");
                if sock.write_all(&frame).await.is_err() {
                    return;
                }
                let _ = sock.flush().await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            match end {
                Finish::Clean => {
                    let _ = sock.write_all(b"0\r\n\r\n").await;
                    let _ = sock.shutdown().await;
                }
                Finish::Disconnect => drop(sock),
                Finish::Hold => {
                    let mut tmp = [0u8; 256];
                    loop {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                    client_closed.notify_one();
                }
            }
        }
    }
}
