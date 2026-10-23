// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A small HTTP/1.1 mock server on loopback, built on tokio only, and the
//! TEST-ONLY signing helpers.
//!
//! It records method, path, query and lowercased headers of every request and
//! answers per path. Every connection is closed after one reply.

#![allow(dead_code, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sqler_update::{Envelope, TrustedKey, signing_message};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};

/// TEST ONLY, never a release key.
pub const TEST_ONLY_SEED_A: [u8; 32] = [0xA1; 32];
/// TEST ONLY, never a release key.
pub const TEST_ONLY_SEED_B: [u8; 32] = [0xB2; 32];

fn pair(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
}

/// The trusted key for a TEST ONLY seed.
pub fn trusted(seed: &[u8; 32]) -> TrustedKey {
    TrustedKey::from_base64(&STANDARD.encode(pair(seed).public_key().as_ref())).unwrap()
}

/// An envelope as bytes: `payload` signed with `seed`, named with `key_id`.
pub fn envelope_bytes(manifest_json: &str, seed: &[u8; 32], key_id: &str) -> Vec<u8> {
    let sig = pair(seed).sign(&signing_message(manifest_json.as_bytes()));
    serde_json::to_vec(&Envelope {
        format: 1,
        key_id: key_id.to_owned(),
        signature: STANDARD.encode(sig.as_ref()),
        payload: STANDARD.encode(manifest_json.as_bytes()),
    })
    .unwrap()
}

/// An envelope signed with `seed` and carrying that seed's own key id.
pub fn signed_envelope(manifest_json: &str, seed: &[u8; 32]) -> Vec<u8> {
    envelope_bytes(manifest_json, seed, trusted(seed).id())
}

#[derive(Clone)]
pub enum Body {
    Full(Vec<u8>),
    /// Send nothing after reading the request; wait for the client to close.
    Stall,
    /// Send these bytes verbatim, then close.
    Raw(Vec<u8>),
}

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Reply {
    pub fn ok(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Body::Full(body),
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Body::Full(Vec::new()),
        }
    }

    pub fn redirect(status: u16, location: &str) -> Self {
        Self {
            status,
            headers: vec![("Location".into(), location.into())],
            body: Body::Full(Vec::new()),
        }
    }

    pub fn stall() -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: Body::Stall,
        }
    }

    /// A complete raw HTTP response, for example one with a lying
    /// `Content-Length`.
    pub fn raw(bytes: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: Body::Raw(bytes),
        }
    }
}

#[derive(Clone)]
pub struct Route {
    pub path: &'static str,
    pub reply: Reply,
}

impl Route {
    pub fn new(path: &'static str, reply: Reply) -> Self {
        Self { path, reply }
    }
}

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    /// The path without the query string.
    pub path: String,
    pub query: Option<String>,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
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

    /// The base URL without a trailing slash.
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
    let target = first.next()?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), Some(q.to_owned())),
        None => (target.to_owned(), None),
    };
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Some(Recorded {
        method,
        path,
        query,
        headers,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

async fn serve(
    mut sock: TcpStream,
    routes: Arc<Vec<Route>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    client_closed: Arc<Notify>,
) {
    use std::fmt::Write as _;
    let _ = sock.set_nodelay(true);
    let Some(req) = read_request(&mut sock).await else {
        return;
    };
    let reply = routes
        .iter()
        .find(|r| r.path == req.path)
        .map_or_else(|| Reply::status(404), |r| r.reply.clone());
    requests.lock().unwrap().push(req);

    match reply.body {
        Body::Full(bytes) => {
            let mut head = format!(
                "HTTP/1.1 {} {}\r\nConnection: close\r\n",
                reply.status,
                reason(reply.status)
            );
            for (k, v) in &reply.headers {
                write!(head, "{k}: {v}\r\n").unwrap();
            }
            write!(head, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&bytes).await;
            let _ = sock.shutdown().await;
        }
        Body::Raw(bytes) => {
            let _ = sock.write_all(&bytes).await;
            let _ = sock.shutdown().await;
        }
        Body::Stall => {
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
