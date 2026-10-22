// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The streamable HTTP transport: a hyper server whose front service checks the request before
//! protocol layer sees it.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use http::header::{
    ALLOW, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN, WWW_AUTHENTICATE,
};
use http::{HeaderMap, Method, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::MCP_PATH;
use crate::auth::AuthToken;
use crate::error::McpError;
use crate::protocol::{
    Engine, Message, PARSE_ERROR, SUPPORTED_VERSIONS, Session, classify, error_response,
};
use crate::server::McpServer;

type Body = BoxBody<Bytes, Infallible>;

/// How long a client may take to send its request head.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long open connections get to finish after shutdown before they are dropped.
const GRACE: Duration = Duration::from_secs(2);

/// The header that carries the session id.
const SESSION_HEADER: &str = "Mcp-Session-Id";
/// The header that carries the negotiated protocol revision.
const VERSION_HEADER: &str = "MCP-Protocol-Version";
/// How many sessions may be open at once.
const MAX_SESSIONS: usize = 64;
/// A session unused for this long is dropped when a new one is created.
const SESSION_IDLE: Duration = Duration::from_mins(30);

/// A bound HTTP endpoint, ready to run.
pub struct HttpServer {
    listener: TcpListener,
    addr: SocketAddr,
    front: Arc<Front>,
    stop: CancellationToken,
}

impl std::fmt::Debug for HttpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpServer")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

struct Front {
    engine: Engine,
    sessions: Sessions,
    token: AuthToken,
    port: u16,
    stop: CancellationToken,
}

/// The live `Mcp-Session-Id` sessions.
#[derive(Default)]
struct Sessions {
    map: Mutex<HashMap<String, Entry>>,
}

struct Entry {
    session: Arc<Session>,
    last_used: Instant,
}

impl Sessions {
    /// A new session, or `None` when the table is full of live sessions.
    fn create(&self) -> Option<(String, Arc<Session>)> {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        map.retain(|_, e| now.duration_since(e.last_used) < SESSION_IDLE);
        if map.len() >= MAX_SESSIONS {
            return None;
        }
        let mut bytes = [0_u8; 16];
        SystemRandom::new().fill(&mut bytes).ok()?;
        let id = URL_SAFE_NO_PAD.encode(bytes);
        let session = Arc::new(Session::default());
        map.insert(
            id.clone(),
            Entry {
                session: Arc::clone(&session),
                last_used: now,
            },
        );
        Some((id, session))
    }

    fn get(&self, id: &str) -> Option<Arc<Session>> {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = map.get_mut(id)?;
        entry.last_used = Instant::now();
        Some(Arc::clone(&entry.session))
    }

    fn remove(&self, id: &str) -> bool {
        let removed = self
            .map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        removed.is_some_and(|e| {
            e.session.cancel_all();
            true
        })
    }
}

impl McpServer {
    /// Binds `addr` (loopback only) and prepares the HTTP endpoint at [`MCP_PATH`].
    ///
    /// # Errors
    ///
    /// `McpError::NonLoopbackBind` for a non-loopback address (nothing is bound);
    /// `McpError::Bind` when the socket cannot be bound.
    pub async fn bind_http(
        self,
        addr: SocketAddr,
        token: AuthToken,
    ) -> Result<HttpServer, McpError> {
        if !addr.ip().is_loopback() {
            return Err(McpError::NonLoopbackBind(addr));
        }
        let listener = TcpListener::bind(addr).await.map_err(McpError::Bind)?;
        let addr = listener.local_addr().map_err(McpError::Bind)?;
        let stop = CancellationToken::new();
        Ok(HttpServer {
            listener,
            addr,
            front: Arc::new(Front {
                engine: Engine::new(self.shared),
                sessions: Sessions::default(),
                token,
                port: addr.port(),
                stop: stop.clone(),
            }),
            stop,
        })
    }
}

impl HttpServer {
    /// The bound address (the real port when port 0 was asked for).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The endpoint URL a client connects to.
    #[must_use]
    pub fn url(&self) -> String {
        match self.addr {
            SocketAddr::V4(a) => format!("http://{a}{MCP_PATH}"),
            SocketAddr::V6(a) => format!("http://[{}]:{}{MCP_PATH}", a.ip(), a.port()),
        }
    }

    /// Accepts connections until `shutdown` resolves, then ends the sessions and returns.
    ///
    /// # Errors
    ///
    /// `McpError::Bind` when accepting fails for good.
    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), McpError> {
        let Self {
            listener,
            front,
            stop,
            ..
        } = self;
        let mut connections = JoinSet::new();
        tokio::pin!(shutdown);
        let outcome = loop {
            tokio::select! {
                () = &mut shutdown => break Ok(()),
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let front = Arc::clone(&front);
                        connections.spawn(serve_connection(stream, front));
                    }
                    Err(err) if is_transient(&err) => {}
                    Err(err) => break Err(McpError::Bind(err)),
                },
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        };
        stop.cancel();
        let drain = async { while connections.join_next().await.is_some() {} };
        if tokio::time::timeout(GRACE, drain).await.is_err() {
            connections.abort_all();
        }
        outcome
    }
}

fn is_transient(err: &std::io::Error) -> bool {
    use std::io::ErrorKind::{ConnectionAborted, ConnectionReset, Interrupted, WouldBlock};
    matches!(
        err.kind(),
        ConnectionAborted | ConnectionReset | Interrupted | WouldBlock
    )
}

async fn serve_connection(stream: tokio::net::TcpStream, front: Arc<Front>) {
    let service = service_fn(move |request: Request<Incoming>| {
        let front = Arc::clone(&front);
        async move { Ok::<_, Infallible>(front.handle(request).await) }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

impl Front {
    async fn handle(&self, request: Request<Incoming>) -> Response<Body> {
        if request.uri().path() != MCP_PATH {
            return reply(StatusCode::NOT_FOUND, "not found");
        }
        if !host_allowed(request.headers(), self.port) || !origin_allowed(request.headers()) {
            return reply(StatusCode::FORBIDDEN, "forbidden");
        }
        if !authorized(request.headers(), &self.token) {
            let mut response = reply(StatusCode::UNAUTHORIZED, "unauthorized");
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, http::HeaderValue::from_static("Bearer"));
            return response;
        }
        let limit = self.engine.max_request_bytes();
        if declared_length(request.headers()).is_some_and(|n| n > limit as u64) {
            return reply(StatusCode::PAYLOAD_TOO_LARGE, "request too large");
        }
        if !version_header_ok(request.headers()) {
            return reply(StatusCode::BAD_REQUEST, "unsupported MCP-Protocol-Version");
        }
        let session_id = request
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        match *request.method() {
            Method::POST => self.post(request, session_id, limit).await,
            Method::DELETE => self.delete(session_id.as_deref()),
            _ => {
                let mut response = reply(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
                response
                    .headers_mut()
                    .insert(ALLOW, http::HeaderValue::from_static("POST, DELETE"));
                response
            }
        }
    }

    fn delete(&self, session_id: Option<&str>) -> Response<Body> {
        match session_id {
            None => reply(StatusCode::BAD_REQUEST, "missing session"),
            Some(id) if self.sessions.remove(id) => reply(StatusCode::NO_CONTENT, ""),
            Some(_) => reply(StatusCode::NOT_FOUND, "unknown session"),
        }
    }

    async fn post(
        &self,
        request: Request<Incoming>,
        session_id: Option<String>,
        limit: usize,
    ) -> Response<Body> {
        let body = match Limited::new(request.into_body(), limit).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err)
                if err
                    .downcast_ref::<http_body_util::LengthLimitError>()
                    .is_some() =>
            {
                return reply(StatusCode::PAYLOAD_TOO_LARGE, "request too large");
            }
            Err(_) => return reply(StatusCode::BAD_REQUEST, "bad request"),
        };
        let Ok(value) = serde_json::from_slice::<Value>(&body) else {
            return json_reply(
                StatusCode::BAD_REQUEST,
                &error_response(&Value::Null, PARSE_ERROR, "parse error"),
                None,
            );
        };
        match classify(value) {
            Message::Invalid(error) => json_reply(StatusCode::BAD_REQUEST, &error, None),
            Message::Request { id, method, params } if method == "initialize" => {
                // Validate first: a bad `initialize` must not take one of the session slots.
                if let Err(error) = Engine::initialize_version(&id, params.as_ref()) {
                    return json_reply(StatusCode::OK, &error, None);
                }
                let Some((new_id, session)) = self.sessions.create() else {
                    return reply(StatusCode::SERVICE_UNAVAILABLE, "too many sessions");
                };
                let answer = self.engine.quick(&session, &id, &method, params.as_ref());
                json_reply(StatusCode::OK, &answer, Some(&new_id))
            }
            message => {
                let Some(session_id) = session_id else {
                    return reply(StatusCode::BAD_REQUEST, "missing session");
                };
                let Some(session) = self.sessions.get(&session_id) else {
                    return reply(StatusCode::NOT_FOUND, "unknown session");
                };
                match message {
                    Message::Request { id, method, params } if method == "tools/call" => {
                        let answer = self
                            .engine
                            .call_tool(&session, &id, params.as_ref(), &self.stop)
                            .await
                            .unwrap_or_else(|| Engine::cancelled_response(&id));
                        json_reply(StatusCode::OK, &answer, None)
                    }
                    Message::Request { id, method, params } => {
                        let answer = self.engine.quick(&session, &id, &method, params.as_ref());
                        json_reply(StatusCode::OK, &answer, None)
                    }
                    Message::Notification { method, params } => {
                        Engine::notify(&session, &method, params.as_ref());
                        reply(StatusCode::ACCEPTED, "")
                    }
                    Message::Response | Message::Invalid(_) => reply(StatusCode::ACCEPTED, ""),
                }
            }
        }
    }
}

/// An `MCP-Protocol-Version` header, when present, must name a revision this server speaks.
fn version_header_ok(headers: &HeaderMap) -> bool {
    headers
        .get(VERSION_HEADER)
        .is_none_or(|v| v.to_str().is_ok_and(|v| SUPPORTED_VERSIONS.contains(&v)))
}

fn json_reply(status: StatusCode, value: &Value, session: Option<&str>) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    if let Some(id) = session
        && let Ok(value) = http::HeaderValue::from_str(id)
    {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}

fn reply(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())).boxed());
    *response.status_mut() = status;
    response
}

fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The host part of an authority and its optional port, for loopback names only.
fn split_authority(authority: &str) -> Option<(&str, Option<u16>)> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(p),
            None if tail.is_empty() => None,
            None => return None,
        };
        (host, port)
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok()?),
        None => None,
    };
    let loopback = host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost");
    loopback.then_some((host, port))
}

/// `Host` must be a loopback name with the bound port (DNS rebinding guard).
pub(crate) fn host_allowed(headers: &HeaderMap, port: u16) -> bool {
    let mut values = headers.get_all(HOST).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    // A bare `::1` without brackets is not a valid authority.
    matches!(split_authority(text), Some((_, Some(p))) if p == port)
}

/// An `Origin`, when present, must be a loopback http(s) origin.
pub(crate) fn origin_allowed(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(ORIGIN).iter();
    let Some(value) = values.next() else {
        return true;
    };
    if values.next().is_some() {
        return false;
    }
    let Ok(text) = value.to_str() else {
        return false;
    };
    let Some(authority) = text
        .strip_prefix("http://")
        .or_else(|| text.strip_prefix("https://"))
    else {
        return false;
    };
    split_authority(authority).is_some()
}

/// The bearer token of the single `Authorization` header, compared in constant time.
pub(crate) fn authorized(headers: &HeaderMap, token: &AuthToken) -> bool {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return false;
    };
    let Some(presented) = bearer(value.as_bytes()) else {
        return false;
    };
    token.matches(presented)
}

/// The token after a case-insensitive `Bearer ` scheme.
fn bearer(value: &[u8]) -> Option<&[u8]> {
    const SCHEME: &[u8] = b"bearer ";
    if value.len() <= SCHEME.len() || !value[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    Some(&value[SCHEME.len()..])
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn host_table() {
        let ok = [
            "127.0.0.1:8080",
            "localhost:8080",
            "LocalHost:8080",
            "[::1]:8080",
        ];
        for host in ok {
            assert!(host_allowed(&headers(&[("host", host)]), 8080), "{host}");
        }
        let bad = [
            "127.0.0.1:8081",
            "127.0.0.1",
            "localhost",
            "evil.example:8080",
            "127.0.0.1.evil.example:8080",
            "0.0.0.0:8080",
            "::1:8080",
            "[::1]x:8080",
            "localhost:notaport",
            "",
        ];
        for host in bad {
            assert!(!host_allowed(&headers(&[("host", host)]), 8080), "{host}");
        }
        assert!(!host_allowed(&HeaderMap::new(), 8080));
        let two = headers(&[("host", "localhost:8080"), ("host", "localhost:8080")]);
        assert!(!host_allowed(&two, 8080));
    }

    #[test]
    fn origin_table() {
        assert!(origin_allowed(&HeaderMap::new()));
        for origin in [
            "http://localhost",
            "http://localhost:3000",
            "https://127.0.0.1:9",
            "http://[::1]:5173",
        ] {
            assert!(origin_allowed(&headers(&[("origin", origin)])), "{origin}");
        }
        for origin in [
            "http://evil.example",
            "https://evil.example:8080",
            "null",
            "file://",
            "http://localhost.evil.example",
            "ftp://localhost",
            "localhost",
        ] {
            assert!(!origin_allowed(&headers(&[("origin", origin)])), "{origin}");
        }
    }

    #[test]
    fn bearer_parsing() {
        let token = AuthToken::new("t".repeat(40)).unwrap();
        let good = format!("Bearer {}", "t".repeat(40));
        assert!(authorized(&headers(&[("authorization", &good)]), &token));
        let lower = format!("bearer {}", "t".repeat(40));
        assert!(authorized(&headers(&[("authorization", &lower)]), &token));
        let upper = format!("BEARER {}", "t".repeat(40));
        assert!(authorized(&headers(&[("authorization", &upper)]), &token));
        for bad in [
            "Bearer ".to_owned(),
            "Bearer".to_owned(),
            "Basic dXNlcjpwYXNz".to_owned(),
            "t".repeat(40),
            format!("Bearer {}", "t".repeat(39)),
            format!("Bearer  {}", "t".repeat(40)),
            format!("Bearer {} ", "t".repeat(40)),
        ] {
            assert!(
                !authorized(&headers(&[("authorization", &bad)]), &token),
                "{bad}"
            );
        }
        assert!(!authorized(&HeaderMap::new(), &token));
        let twice = headers(&[("authorization", &good), ("authorization", &good)]);
        assert!(!authorized(&twice, &token));
    }
}
