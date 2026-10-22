// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The stdio transport over an in-memory pipe, driven with raw JSON-RPC lines.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{SqliteSource, server, slow_sql};
use serde_json::{Value, json};
use sqler_ai_tools::{MemoryAudit, Surface, names, unwrap_untrusted};
use sqler_mcp::{McpError, McpServer, ServerOptions, UI_ONLY_TOOLS};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::task::JoinHandle;

type Task = JoinHandle<Result<(), McpError>>;

/// The client end of the pipe.
struct Wire {
    tx: WriteHalf<DuplexStream>,
    rx: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next: i64,
}

impl Wire {
    async fn send(&mut self, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.tx.write_all(line.as_bytes()).await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(30), self.rx.next_line())
            .await
            .expect("a line in time")
            .unwrap()
            .expect("the server closed the pipe");
        serde_json::from_str(&line).unwrap()
    }

    /// Sends a request and returns the answer with the same id.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let reply = self.recv().await;
            if reply["id"] == id {
                return reply;
            }
        }
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }

    async fn handshake(&mut self) -> Value {
        let reply = self
            .request(
                "initialize",
                json!({"protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"}}),
            )
            .await;
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        reply
    }
}

fn pipe(server: McpServer) -> (Wire, Task) {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);
    let task = tokio::spawn(server.serve_io(server_read, server_write, std::future::pending()));
    let wire = Wire {
        tx: client_write,
        rx: BufReader::new(client_read).lines(),
        next: 0,
    };
    (wire, task)
}

/// The JSON a tool call carries inside the untrusted-data envelope.
fn payload(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(unwrap_untrusted(text).unwrap()).unwrap()
}

fn rows(reply: &Value) -> Value {
    payload(reply)["statements"][0]["rows"].clone()
}

async fn setup(tag: &str) -> (Arc<SqliteSource>, Wire, Arc<MemoryAudit>, Task) {
    let source = SqliteSource::new(tag).await;
    let (server, audit) = server(&source, ServerOptions::default());
    let (mut wire, task) = pipe(server);
    wire.handshake().await;
    (source, wire, audit, task)
}

#[tokio::test]
async fn handshake_reports_the_server_and_tools_capability() {
    let source = SqliteSource::new("hello").await;
    let (server, _audit) = server(&source, ServerOptions::default());
    let (mut wire, _task) = pipe(server);
    let reply = wire.handshake().await;
    let result = &reply["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert_eq!(result["serverInfo"]["name"], "sqler");
    assert_eq!(
        result["capabilities"],
        json!({"tools": {"listChanged": false}})
    );
    // An older revision is echoed, an unknown one gets the newest we speak.
    let old = wire
        .request("initialize", json!({"protocolVersion": "2025-03-26"}))
        .await;
    assert_eq!(old["result"]["protocolVersion"], "2025-03-26");
    let future = wire
        .request("initialize", json!({"protocolVersion": "2099-01-01"}))
        .await;
    assert_eq!(future["result"]["protocolVersion"], "2025-11-25");
    let ping = wire.request("ping", json!({})).await;
    assert_eq!(ping["result"], json!({}));
}

#[tokio::test]
async fn tools_need_the_handshake_first() {
    let source = SqliteSource::new("early").await;
    let (server, _audit) = server(&source, ServerOptions::default());
    let (mut wire, _task) = pipe(server);
    let reply = wire.request("tools/list", json!({})).await;
    assert_eq!(reply["error"]["code"], -32600);
    let reply = wire.call(names::LIST_CONNECTIONS, json!({})).await;
    assert_eq!(reply["error"]["code"], -32600);
}

#[tokio::test]
async fn tools_list_matches_the_tool_set_without_ui_only_tools() {
    let source = SqliteSource::new("list").await;
    let (server, _audit) = server(&source, ServerOptions::default());
    let expected = server.tool_definitions().to_vec();
    let (mut wire, _task) = pipe(server);
    wire.handshake().await;
    let reply = wire.request("tools/list", json!({})).await;
    let tools = reply["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), expected.len());
    for (tool, definition) in tools.iter().zip(&expected) {
        assert_eq!(tool["name"], definition.name.as_str());
        assert_eq!(tool["description"], definition.description.as_str());
        assert_eq!(tool["inputSchema"], definition.parameters);
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
    // The tool set itself (with a UI host) lists the UI-only tools; the server must not.
    let with_ui = sqler_ai_tools::definitions(true);
    for name in UI_ONLY_TOOLS {
        assert!(with_ui.iter().any(|d| d.name == name));
        assert!(tools.iter().all(|t| t["name"] != name));
    }
    let base: Vec<_> = with_ui
        .iter()
        .filter(|d| !UI_ONLY_TOOLS.contains(&d.name.as_str()))
        .map(|d| d.name.clone())
        .collect();
    let listed: Vec<_> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(listed, base);
}

#[tokio::test]
async fn bad_requests_get_protocol_errors_and_no_dispatch() {
    let (_source, mut wire, audit, _task) = setup("errors").await;
    for name in [
        names::OPEN_IN_EDITOR,
        names::OPEN_RESULTS_TAB,
        "no_such_tool",
    ] {
        let reply = wire.call(name, json!({"sql": "select 1"})).await;
        assert_eq!(reply["error"]["code"], -32602, "{name}");
    }
    let reply = wire.request("tools/call", json!({})).await;
    assert_eq!(reply["error"]["code"], -32602);
    let reply = wire
        .request("tools/call", json!({"name": "run_query", "arguments": [1]}))
        .await;
    assert_eq!(reply["error"]["code"], -32602);
    let reply = wire.request("resources/list", json!({})).await;
    assert_eq!(reply["error"]["code"], -32601);
    // Garbage lines are answered, not fatal.
    wire.tx.write_all(b"not json\n").await.unwrap();
    assert_eq!(wire.recv().await["error"]["code"], -32700);
    wire.send(&json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}]))
        .await;
    assert_eq!(wire.recv().await["error"]["code"], -32600);
    wire.send(&json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}))
        .await;
    assert_eq!(wire.recv().await["error"]["code"], -32600);
    assert_eq!(wire.request("ping", json!({})).await["result"], json!({}));
    assert!(audit.entries().is_empty(), "no dispatch, no audit entry");
}

#[tokio::test]
async fn catalog_tools_answer() {
    let (_source, mut wire, _audit, _task) = setup("catalog").await;
    let reply = wire.call(names::LIST_CONNECTIONS, json!({})).await;
    assert_eq!(reply["result"]["isError"], false);
    let text = reply.to_string();
    assert!(text.contains("\\\"db\\\""));
    assert!(!text.contains(".db"), "no target in the answer");
    let reply = wire
        .call(names::LIST_TABLES, json!({"connection": "db"}))
        .await;
    assert!(reply.to_string().contains("users"));
    let reply = wire
        .call(
            names::DESCRIBE_TABLE,
            json!({"connection": "db", "name": "users"}),
        )
        .await;
    assert!(reply.to_string().contains("name"));
}

#[tokio::test]
async fn read_query_returns_enveloped_rows_and_audits_as_mcp() {
    let (_source, mut wire, audit, _task) = setup("read").await;
    let reply = wire
        .call(
            names::RUN_QUERY,
            json!({"connection": "db", "sql": "select id, name from users order by id"}),
        )
        .await;
    assert_eq!(reply["result"]["isError"], false);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        unwrap_untrusted(text).is_some(),
        "the envelope is unchanged"
    );
    assert_eq!(rows(&reply), json!([[1, "ann"], [2, "bob"], [3, "cy"]]));
    let entries = audit.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].surface, Surface::Mcp);
    assert_eq!(entries[0].tool, names::RUN_QUERY);
}

#[tokio::test]
async fn writes_are_refused_without_an_approver() {
    let (_source, mut wire, audit, _task) = setup("write").await;
    let reply = wire
        .call(
            names::RUN_QUERY,
            json!({"connection": "db", "sql": "insert into users (id, name) values (9, 'zed')"}),
        )
        .await;
    assert_eq!(reply["result"]["isError"], true);
    assert!(reply.to_string().contains("no_approver"), "{reply}");
    let count = wire
        .call(
            names::RUN_QUERY,
            json!({"connection": "db", "sql": "select count(*) as n from users"}),
        )
        .await;
    assert_eq!(rows(&count), json!([[3]]), "the table is unchanged");
    let entries = audit.entries();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.surface == Surface::Mcp));
}

#[tokio::test]
async fn concurrent_calls_get_their_own_answers() {
    let (_source, mut wire, audit, _task) = setup("conc").await;
    for i in 1..=8_i64 {
        let sql = format!("select {i} as marker");
        wire.send(
            &json!({"jsonrpc": "2.0", "id": 100 + i, "method": "tools/call", "params": {
            "name": "run_query", "arguments": {"connection": "db", "sql": sql}}}),
        )
        .await;
    }
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..8 {
        let reply = wire.recv().await;
        let id = reply["id"].as_i64().unwrap();
        assert_eq!(rows(&reply), json!([[id - 100]]), "answer for {id}");
        seen.insert(id);
    }
    assert_eq!(seen.len(), 8);
    assert_eq!(audit.entries().len(), 8);
}

#[tokio::test]
async fn a_cancelled_call_yields_no_result_and_frees_its_permit() {
    let source = SqliteSource::new("cancel").await;
    let mut options = ServerOptions::default();
    options.max_concurrent_calls = 1;
    let (server, audit) = server(&source, options);
    let (mut wire, _task) = pipe(server);
    wire.handshake().await;

    wire.send(
        &json!({"jsonrpc": "2.0", "id": "slow", "method": "tools/call", "params": {
        "name": "run_query", "arguments": {"connection": "db", "sql": slow_sql()}}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(audit.entries().is_empty(), "the slow call is still running");
    wire.send(
        &json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": "slow", "reason": "test"}}),
    )
    .await;

    // With a single permit this call can only finish if the cancelled one let go of it.
    let next = wire
        .call(
            names::RUN_QUERY,
            json!({"connection": "db", "sql": "select 7 as seven"}),
        )
        .await;
    assert_eq!(rows(&next), json!([[7]]));
    let mut outcomes: Vec<&str> = audit.entries().iter().map(|e| e.outcome.as_str()).collect();
    outcomes.sort_unstable();
    assert_eq!(
        outcomes.iter().filter(|o| **o == "cancelled").count(),
        1,
        "the cancelled call is audited as cancelled: {outcomes:?}"
    );
    assert_eq!(outcomes.len(), 2, "plus the finished call: {outcomes:?}");
    // And nothing was ever sent for the cancelled request.
    let extra = tokio::time::timeout(Duration::from_millis(300), wire.rx.next_line()).await;
    assert!(extra.is_err(), "unexpected output: {extra:?}");
}

#[tokio::test]
async fn an_over_cap_line_ends_the_session() {
    let source = SqliteSource::new("cap").await;
    let mut options = ServerOptions::default();
    options.max_request_bytes = 256;
    let (server, _audit) = server(&source, options);
    let (mut wire, task) = pipe(server);
    let long = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}\n",
        "x".repeat(1000)
    );
    wire.tx.write_all(long.as_bytes()).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(outcome, Err(McpError::RequestTooLarge)),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn end_of_input_lets_running_calls_finish_and_shutdown_ends_cleanly() {
    let source = SqliteSource::new("eof").await;
    let (server, _audit) = server(&source, ServerOptions::default());
    let (mut wire, task) = pipe(server);
    wire.handshake().await;
    wire.send(
        &json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
        "name": "run_query", "arguments": {"connection": "db", "sql": "select 5"}}}),
    )
    .await;
    wire.tx.shutdown().await.unwrap();
    // The answer still arrives, then the session ends.
    let reply = wire.recv().await;
    assert_eq!(reply["id"], 5);
    let outcome = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert!(outcome.is_ok(), "{outcome:?}");

    let (server, _audit) = common::server(&source, ServerOptions::default());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (_keep, b) = tokio::io::duplex(1024);
    let (r, w) = tokio::io::split(b);
    let task = tokio::spawn(server.serve_io(r, w, async move {
        let _ = rx.await;
    }));
    tx.send(()).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert!(outcome.is_ok(), "{outcome:?}");
}
