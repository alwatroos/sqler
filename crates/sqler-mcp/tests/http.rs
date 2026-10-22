// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The streamable HTTP transport over loopback, driven with raw HTTP and JSON-RPC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use common::{SqliteSource, server, slow_sql};
use http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sqler_ai_tools::{MemoryAudit, Surface, names, unwrap_untrusted};
use sqler_mcp::{AuthToken, McpError, McpServer, ServerOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

// Captured tracing output, for the "the token is never logged" check.
static LOG: OnceLock<Mutex<String>> = OnceLock::new();

struct Capture;

struct Visitor<'a>(&'a mut String);

impl tracing::field::Visit for Visitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut text = String::new();
        span.record(&mut Visitor(&mut text));
        if let Some(log) = LOG.get() {
            log.lock().unwrap().push_str(&text);
        }
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut text = String::new();
        event.record(&mut Visitor(&mut text));
        if let Some(log) = LOG.get() {
            let mut log = log.lock().unwrap();
            log.push_str(&text);
            log.push('\n');
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn capture_logs() {
    LOG.get_or_init(|| Mutex::new(String::new()));
    let _ = tracing::subscriber::set_global_default(Capture);
}

fn options(change: impl FnOnce(&mut ServerOptions)) -> ServerOptions {
    let mut options = ServerOptions::default();
    change(&mut options);
    options
}

struct Running {
    addr: SocketAddr,
    token: String,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), McpError>>>,
    audit: Arc<MemoryAudit>,
    _source: Arc<SqliteSource>,
}

impl Running {
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        let outcome = tokio::time::timeout(Duration::from_secs(10), self.task.take().unwrap())
            .await
            .expect("the server stops")
            .unwrap();
        assert!(outcome.is_ok());
    }
}

async fn start(tag: &str, options: ServerOptions) -> Running {
    capture_logs();
    let source = SqliteSource::new(tag).await;
    let (server, audit) = server(&source, options);
    start_with(server, audit, source).await
}

async fn start_with(
    server: McpServer,
    audit: Arc<MemoryAudit>,
    source: Arc<SqliteSource>,
) -> Running {
    let token = AuthToken::generate().unwrap();
    let text = token.expose().to_owned();
    let http = server
        .bind_http("127.0.0.1:0".parse().unwrap(), token)
        .await
        .unwrap();
    let addr = http.local_addr();
    assert_eq!(http.url(), format!("http://127.0.0.1:{}/mcp", addr.port()));
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(http.run(async move {
        let _ = rx.await;
    }));
    Running {
        addr,
        token: text,
        stop: Some(tx),
        task: Some(task),
        audit,
        _source: source,
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn send(addr: SocketAddr, request: Request<Full<Bytes>>) -> Reply {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.unwrap();
    tokio::spawn(connection);
    let response = tokio::time::timeout(Duration::from_secs(30), sender.send_request(request))
        .await
        .expect("a response")
        .unwrap();
    let (parts, body) = response.into_parts();
    let body = tokio::time::timeout(Duration::from_secs(30), body.collect())
        .await
        .expect("a finished body")
        .unwrap()
        .to_bytes();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

struct Req<'a> {
    addr: SocketAddr,
    method: Method,
    path: &'a str,
    host: Option<String>,
    origin: Option<&'a str>,
    auth: Option<String>,
    session: Option<&'a str>,
    protocol: Option<&'a str>,
    body: Option<String>,
}

impl Req<'_> {
    fn post(run: &Running, body: &Value) -> Self {
        Self {
            addr: run.addr,
            method: Method::POST,
            path: "/mcp",
            host: Some(run.addr.to_string()),
            origin: None,
            auth: Some(format!("Bearer {}", run.token)),
            session: None,
            protocol: None,
            body: Some(body.to_string()),
        }
    }

    async fn send(self) -> Reply {
        let mut builder = Request::builder()
            .method(self.method)
            .uri(self.path)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        if let Some(host) = self.host {
            builder = builder.header("host", host);
        }
        if let Some(origin) = self.origin {
            builder = builder.header("origin", origin);
        }
        if let Some(auth) = self.auth {
            builder = builder.header("authorization", auth);
        }
        if let Some(session) = self.session {
            builder = builder.header("mcp-session-id", session);
        }
        if let Some(protocol) = self.protocol {
            builder = builder.header("mcp-protocol-version", protocol);
        }
        let body = Full::new(Bytes::from(self.body.unwrap_or_default()));
        send(self.addr, builder.body(body).unwrap()).await
    }
}

/// The JSON-RPC messages of a reply body, whether plain JSON or an event stream.
fn messages(reply: &Reply) -> Vec<Value> {
    let text = reply.body.trim();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return vec![value];
    }
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .collect()
}

fn response_to(reply: &Reply, id: i64) -> Value {
    messages(reply)
        .into_iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("no response {id} in {:?}", reply.body))
}

fn initialize_body() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"}}})
}

/// Initializes a session and returns its id.
async fn open_session(run: &Running) -> String {
    let reply = Req::post(run, &initialize_body()).send().await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let session = reply
        .headers
        .get("mcp-session-id")
        .expect("a session id")
        .to_str()
        .unwrap()
        .to_owned();
    let info = response_to(&reply, 1);
    assert_eq!(info["result"]["serverInfo"]["name"], "sqler");
    assert!(info["result"]["capabilities"]["tools"].is_object());
    let mut note = Req::post(
        run,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    note.session = Some(&session);
    let reply = note.send().await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.body);
    session
}

async fn rpc(run: &Running, session: &str, id: i64, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let mut req = Req::post(run, &body);
    req.session = Some(session);
    let reply = req.send().await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    response_to(&reply, id)
}

fn tool_json(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(unwrap_untrusted(text).unwrap()).unwrap()
}

#[tokio::test]
async fn non_loopback_binds_are_refused() {
    let source = SqliteSource::new("bind").await;
    for addr in ["0.0.0.0:0", "192.0.2.1:8080", "[::]:0"] {
        let (server, _) = server(&source, ServerOptions::default());
        let token = AuthToken::generate().unwrap();
        let err = server
            .bind_http(addr.parse().unwrap(), token)
            .await
            .unwrap_err();
        assert!(
            matches!(err, McpError::NonLoopbackBind(_)),
            "{addr}: {err:?}"
        );
    }
    let (server, _) = server(&source, ServerOptions::default());
    match server
        .bind_http("[::1]:0".parse().unwrap(), AuthToken::generate().unwrap())
        .await
    {
        Ok(http) => assert_eq!(
            http.url(),
            format!("http://[::1]:{}/mcp", http.local_addr().port())
        ),
        Err(McpError::Bind(_)) => eprintln!("no IPv6 loopback here; skipped"),
        Err(other) => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn token_checks() {
    let run = start("auth", ServerOptions::default()).await;
    let body = initialize_body();
    let wrong_same_len = format!("Bearer {}", "A".repeat(run.token.len()));
    let cases: [(&str, Option<String>, StatusCode); 7] = [
        ("missing", None, StatusCode::UNAUTHORIZED),
        (
            "wrong",
            Some("Bearer not-the-token".into()),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "right length, wrong",
            Some(wrong_same_len),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "prefix of right",
            Some(format!("Bearer {}", &run.token[..20])),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "not bearer",
            Some(format!("Basic {}", run.token)),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "bare token",
            Some(run.token.clone()),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "lower-case scheme",
            Some(format!("bearer {}", run.token)),
            StatusCode::OK,
        ),
    ];
    for (name, auth, expected) in cases {
        let mut req = Req::post(&run, &body);
        req.auth = auth;
        let reply = req.send().await;
        assert_eq!(reply.status, expected, "{name}: {}", reply.body);
        if expected == StatusCode::UNAUTHORIZED {
            assert_eq!(reply.headers.get("www-authenticate").unwrap(), "Bearer");
            assert_eq!(reply.body, "unauthorized", "one fixed body");
        }
        assert!(!reply.body.contains(&run.token));
    }
    run.stop().await;
}

#[tokio::test]
async fn path_host_origin_and_size_checks_run_in_order() {
    let run = start("front", options(|o| o.max_request_bytes = 2048)).await;
    let body = initialize_body();

    let mut req = Req::post(&run, &body);
    req.path = "/other";
    req.auth = None;
    assert_eq!(
        req.send().await.status,
        StatusCode::NOT_FOUND,
        "path first, before auth"
    );

    let mut req = Req::post(&run, &body);
    req.host = Some("evil.example".into());
    req.auth = None;
    assert_eq!(
        req.send().await.status,
        StatusCode::FORBIDDEN,
        "host before auth"
    );

    let mut req = Req::post(&run, &body);
    req.host = Some(format!("127.0.0.1:{}", run.addr.port() + 1));
    assert_eq!(req.send().await.status, StatusCode::FORBIDDEN, "wrong port");

    let mut req = Req::post(&run, &body);
    req.origin = Some("https://evil.example");
    assert_eq!(req.send().await.status, StatusCode::FORBIDDEN);

    let mut req = Req::post(&run, &body);
    req.origin = Some("http://localhost:3000");
    assert_eq!(
        req.send().await.status,
        StatusCode::OK,
        "a loopback origin passes"
    );

    let mut req = Req::post(&run, &body);
    req.host = Some(format!("localhost:{}", run.addr.port()));
    assert_eq!(req.send().await.status, StatusCode::OK, "localhost passes");

    let mut big = Req::post(&run, &body);
    big.body = Some("x".repeat(4096));
    assert_eq!(big.send().await.status, StatusCode::PAYLOAD_TOO_LARGE);

    let mut big = Req::post(&run, &body);
    big.body = Some("x".repeat(4096));
    big.auth = None;
    assert_eq!(
        big.send().await.status,
        StatusCode::UNAUTHORIZED,
        "auth before size"
    );

    // A streamed (chunked) body without a Content-Length is capped as well.
    let chunk = "y".repeat(4096);
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{chunk}\r\n0\r\n\r\n",
        run.addr,
        run.token,
        chunk.len()
    );
    let mut stream = TcpStream::connect(run.addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_string(&mut answer)).await;
    assert!(answer.starts_with("HTTP/1.1 413"), "{answer}");

    // Only POST and DELETE are served; there is no server-to-client stream.
    for method in [Method::PUT, Method::GET] {
        let mut other = Req::post(&run, &body);
        other.method = method;
        other.body = None;
        let reply = other.send().await;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(reply.headers.get("allow").unwrap(), "POST, DELETE");
    }
    run.stop().await;
}

#[tokio::test]
async fn full_flow_list_call_refuse_write_and_audit() {
    let run = start("flow", ServerOptions::default()).await;
    let session = open_session(&run).await;

    let listed = rpc(&run, &session, 2, "tools/list", json!({})).await;
    let names_listed: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names_listed.contains(&names::RUN_QUERY));
    assert!(!names_listed.contains(&names::OPEN_IN_EDITOR));
    assert!(!names_listed.contains(&names::OPEN_RESULTS_TAB));
    assert_eq!(
        names_listed.len(),
        sqler_ai_tools::definitions(false).len(),
        "the same set a headless tool set lists"
    );

    let read = rpc(
        &run,
        &session,
        3,
        "tools/call",
        json!({"name": "run_query", "arguments": {"connection": "db", "sql": "select name from users order by id"}}),
    )
    .await;
    assert_eq!(read["result"]["isError"], false);
    assert_eq!(
        tool_json(&read)["statements"][0]["rows"],
        json!([["ann"], ["bob"], ["cy"]])
    );

    let write = rpc(
        &run,
        &session,
        4,
        "tools/call",
        json!({"name": "run_query", "arguments": {"connection": "db", "sql": "delete from users"}}),
    )
    .await;
    assert_eq!(write["result"]["isError"], true);
    assert!(write.to_string().contains("no_approver"));

    let unknown = rpc(
        &run,
        &session,
        5,
        "tools/call",
        json!({"name": "open_in_editor", "arguments": {"sql": "x"}}),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32602);

    let entries = run.audit.entries();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.surface == Surface::Mcp));

    // The session can be ended with DELETE; afterwards its id is unknown.
    let mut delete = Req::post(&run, &json!({}));
    delete.method = Method::DELETE;
    delete.session = Some(&session);
    delete.body = None;
    assert!(delete.send().await.status.is_success());
    let mut again = Req::post(
        &run,
        &json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list", "params": {}}),
    );
    again.session = Some(&session);
    assert_eq!(again.send().await.status, StatusCode::NOT_FOUND);
    run.stop().await;
}

#[tokio::test]
async fn session_and_protocol_header_rules() {
    let run = start("rules", ServerOptions::default()).await;
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}});

    let reply = Req::post(&run, &list).send().await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "no session id");
    let mut req = Req::post(&run, &list);
    req.session = Some("does-not-exist");
    assert_eq!(req.send().await.status, StatusCode::NOT_FOUND);

    let session = open_session(&run).await;
    let mut req = Req::post(&run, &list);
    req.session = Some(&session);
    req.protocol = Some("1999-01-01");
    assert_eq!(req.send().await.status, StatusCode::BAD_REQUEST);
    let mut req = Req::post(&run, &list);
    req.session = Some(&session);
    req.protocol = Some("2025-11-25");
    assert_eq!(req.send().await.status, StatusCode::OK);

    let mut junk = Req::post(&run, &list);
    junk.body = Some("not json".into());
    junk.session = Some(&session);
    let reply = junk.send().await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(response_to_null(&reply)["error"]["code"], -32700);
    run.stop().await;
}

fn response_to_null(reply: &Reply) -> Value {
    serde_json::from_str(&reply.body).unwrap()
}

#[tokio::test]
async fn the_number_of_sessions_is_capped() {
    let run = start("cap", ServerOptions::default()).await;
    let mut last = StatusCode::OK;
    let mut opened = 0;
    for _ in 0..100 {
        let reply = Req::post(&run, &initialize_body()).send().await;
        last = reply.status;
        if last != StatusCode::OK {
            break;
        }
        opened += 1;
    }
    assert_eq!(last, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(opened, 64);
    run.stop().await;
}

#[tokio::test]
async fn a_bad_initialize_does_not_take_a_session_slot() {
    let run = start("badinit", ServerOptions::default()).await;
    for _ in 0..100 {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let reply = Req::post(&run, &body).send().await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        assert!(reply.headers.get("mcp-session-id").is_none());
        assert_eq!(response_to(&reply, 1)["error"]["code"], -32602);
    }
    let session = open_session(&run).await;
    assert!(!session.is_empty(), "the slots are all still free");
    run.stop().await;
}

#[tokio::test]
async fn concurrent_calls_over_http() {
    let run = Arc::new(start("conc", ServerOptions::default()).await);
    let session = open_session(&run).await;
    let mut tasks = Vec::new();
    for i in 1..=8_i64 {
        let run = run.clone();
        let session = session.clone();
        tasks.push(tokio::spawn(async move {
            let sql = format!("select {i} as marker");
            let reply = rpc(
                &run,
                &session,
                10 + i,
                "tools/call",
                json!({"name": "run_query", "arguments": {"connection": "db", "sql": sql}}),
            )
            .await;
            (i, reply)
        }));
    }
    for task in tasks {
        let (i, reply) = task.await.unwrap();
        assert_eq!(tool_json(&reply)["statements"][0]["rows"], json!([[i]]));
    }
    assert_eq!(run.audit.entries().len(), 8);
    let run = Arc::into_inner(run).unwrap();
    run.stop().await;
}

#[tokio::test]
async fn a_cancelled_http_call_ends_without_a_result_and_frees_the_permit() {
    let run = Arc::new(start("cancel", options(|o| o.max_concurrent_calls = 1)).await);
    let session = open_session(&run).await;

    let slow = {
        let run = run.clone();
        let session = session.clone();
        tokio::spawn(async move {
            let body = json!({"jsonrpc": "2.0", "id": 50, "method": "tools/call", "params": {
                "name": "run_query", "arguments": {"connection": "db", "sql": slow_sql()}}});
            let mut req = Req::post(&run, &body);
            req.session = Some(&session);
            req.send().await
        })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        run.audit.entries().is_empty(),
        "the slow call is still running"
    );
    let note = json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": 50, "reason": "test"}});
    let mut req = Req::post(&run, &note);
    req.session = Some(&session);
    assert_eq!(req.send().await.status, StatusCode::ACCEPTED);

    let next = tokio::time::timeout(
        Duration::from_secs(20),
        rpc(
            &run,
            &session,
            51,
            "tools/call",
            json!({"name": "run_query", "arguments": {"connection": "db", "sql": "select 7 as seven"}}),
        ),
    )
    .await
    .expect("the permit was freed");
    assert_eq!(tool_json(&next)["statements"][0]["rows"], json!([[7]]));

    let mut outcomes: Vec<&str> = run
        .audit
        .entries()
        .iter()
        .map(|e| e.outcome.as_str())
        .collect();
    outcomes.sort_unstable();
    assert_eq!(
        outcomes.iter().filter(|o| **o == "cancelled").count(),
        1,
        "the cancelled call is audited as cancelled: {outcomes:?}"
    );
    assert_eq!(outcomes.len(), 2, "plus the finished call: {outcomes:?}");
    let reply = tokio::time::timeout(Duration::from_secs(20), slow)
        .await
        .unwrap()
        .unwrap();
    assert!(
        messages(&reply)
            .iter()
            .all(|m| m["id"] != 50 || m.get("result").is_none()),
        "no result for the cancelled call: {}",
        reply.body
    );
    let run = Arc::into_inner(run).unwrap();
    run.stop().await;
}

#[tokio::test]
async fn a_dropped_connection_cancels_the_call() {
    let run = start("hangup", options(|o| o.max_concurrent_calls = 1)).await;
    let session = open_session(&run).await;
    let body = json!({"jsonrpc": "2.0", "id": 60, "method": "tools/call", "params": {
        "name": "run_query", "arguments": {"connection": "db", "sql": slow_sql()}}})
    .to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nMcp-Session-Id: {session}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        run.addr,
        run.token,
        body.len()
    );
    let mut stream = TcpStream::connect(run.addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        run.audit.entries().is_empty(),
        "the slow call is still running"
    );
    drop(stream);

    let next = tokio::time::timeout(
        Duration::from_secs(20),
        rpc(
            &run,
            &session,
            61,
            "tools/call",
            json!({"name": "run_query", "arguments": {"connection": "db", "sql": "select 8 as eight"}}),
        ),
    )
    .await
    .expect("the permit was freed");
    assert_eq!(tool_json(&next)["statements"][0]["rows"], json!([[8]]));
    run.stop().await;
}

#[tokio::test]
async fn the_token_is_never_echoed_or_logged() {
    let run = start("secret", ServerOptions::default()).await;
    let session = open_session(&run).await;
    let mut bodies = String::new();
    let reply = rpc(&run, &session, 2, "tools/list", json!({})).await;
    bodies.push_str(&reply.to_string());
    let reply = rpc(
        &run,
        &session,
        3,
        "tools/call",
        json!({"name": "run_query", "arguments": {"connection": "db", "sql": "select 1"}}),
    )
    .await;
    bodies.push_str(&reply.to_string());
    let mut bad = Req::post(&run, &initialize_body());
    bad.auth = Some(format!("Bearer {}x", run.token));
    let denied = bad.send().await;
    bodies.push_str(&denied.body);
    for (name, value) in &denied.headers {
        let _ = writeln!(bodies, "{name}: {value:?}");
    }
    assert!(!bodies.contains(&run.token));
    let token = run.token.clone();
    run.stop().await;
    let log = LOG.get().unwrap().lock().unwrap().clone();
    assert!(!log.contains(&token), "the token reached the logs");
    assert!(
        !log.to_ascii_lowercase().contains("bearer"),
        "the credential header reached the logs"
    );
}
