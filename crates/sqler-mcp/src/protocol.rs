// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The MCP protocol layer: JSON-RPC 2.0 messages in, messages out, independent of the transport.
//!
//! Supported methods: `initialize`, `ping`, `tools/list`, `tools/call`, and the notifications
//! `notifications/initialized` and `notifications/cancelled`. Everything else is `-32601`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value, json};
use sqler_ai_api::ToolCall;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::map;
use crate::server::Shared;

/// Protocol revisions this server speaks, newest first. All use the `initialize` handshake.
pub(crate) const SUPPORTED_VERSIONS: [&str; 4] =
    ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const INVALID_REQUEST: i64 = -32600;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;
pub(crate) const INTERNAL_ERROR: i64 = -32603;
/// A request that was cancelled while its HTTP response was still owed.
pub(crate) const CANCELLED: i64 = -32800;

/// The text of every internal failure; nothing from the cause reaches the client.
const INTERNAL: &str = "internal error";

/// One client connection: the stdio session, or one `Mcp-Session-Id`.
#[derive(Default)]
pub(crate) struct Session {
    initialized: AtomicBool,
    inflight: Mutex<HashMap<String, CancellationToken>>,
}

impl Session {
    /// Whether `initialize` was answered.
    pub(crate) fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::SeqCst)
    }

    /// Cancels every request still running.
    pub(crate) fn cancel_all(&self) {
        let map = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
        for token in map.values() {
            token.cancel();
        }
    }

    fn register(&self, id: &Value) -> Option<Flight<'_>> {
        let key = id.to_string();
        let token = CancellationToken::new();
        let mut map = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
        if map.contains_key(&key) {
            return None;
        }
        map.insert(key.clone(), token.clone());
        Some(Flight {
            session: self,
            key,
            token,
        })
    }

    fn cancel(&self, id: &Value) {
        let map = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(token) = map.get(&id.to_string()) {
            token.cancel();
        }
    }
}

/// A registered in-flight request; unregisters on drop.
struct Flight<'a> {
    session: &'a Session,
    key: String,
    token: CancellationToken,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.session
            .inflight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

/// Aborts a spawned task when dropped, which drops the future it runs.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What one incoming JSON-RPC value is.
pub(crate) enum Message {
    /// A request that wants an answer.
    Request {
        id: Value,
        method: String,
        params: Option<Value>,
    },
    /// A notification; never answered.
    Notification {
        method: String,
        params: Option<Value>,
    },
    /// A response from the client; the server sends no requests, so it is ignored.
    Response,
    /// Not a valid message; answer with this error.
    Invalid(Value),
}

/// The error response for `id`.
pub(crate) fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[allow(clippy::needless_pass_by_value)]
fn result_response(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Reads the shape of one JSON-RPC value.
pub(crate) fn classify(value: Value) -> Message {
    let Value::Object(mut object) = value else {
        return Message::Invalid(error_response(
            &Value::Null,
            INVALID_REQUEST,
            "a message must be a JSON object (batches are not supported)",
        ));
    };
    let id = object.remove("id");
    let method = object.remove("method");
    let params = object.remove("params");
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Message::Invalid(error_response(
            id.as_ref().filter(|i| valid_id(i)).unwrap_or(&Value::Null),
            INVALID_REQUEST,
            "jsonrpc must be \"2.0\"",
        ));
    }
    match (id, method) {
        (Some(id), Some(Value::String(method))) if valid_id(&id) => {
            Message::Request { id, method, params }
        }
        (None, Some(Value::String(method))) => Message::Notification { method, params },
        (Some(id), None)
            if valid_id(&id) && (object.contains_key("result") || object.contains_key("error")) =>
        {
            Message::Response
        }
        _ => Message::Invalid(error_response(
            &Value::Null,
            INVALID_REQUEST,
            "not a valid JSON-RPC message",
        )),
    }
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.is_number()
}

/// The protocol engine of one server.
#[derive(Clone)]
pub(crate) struct Engine {
    shared: Arc<Shared>,
}

impl Engine {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    pub(crate) fn max_request_bytes(&self) -> usize {
        self.shared.options.max_request_bytes
    }

    /// Handles a notification.
    pub(crate) fn notify(session: &Session, method: &str, params: Option<&Value>) {
        if method == "notifications/cancelled"
            && let Some(id) = params.and_then(|p| p.get("requestId"))
        {
            session.cancel(id);
        }
        // `notifications/initialized` and unknown notifications need no action.
    }

    /// Answers every request except `tools/call` (which can run long; see [`Engine::call_tool`]).
    pub(crate) fn quick(
        &self,
        session: &Session,
        id: &Value,
        method: &str,
        params: Option<&Value>,
    ) -> Value {
        match method {
            "initialize" => {
                let asked = params
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str);
                let Some(asked) = asked else {
                    return error_response(id, INVALID_PARAMS, "protocolVersion is required");
                };
                let version = SUPPORTED_VERSIONS
                    .iter()
                    .find(|v| **v == asked)
                    .unwrap_or(&SUPPORTED_VERSIONS[0]);
                session.initialized.store(true, Ordering::SeqCst);
                result_response(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {
                            "name": self.shared.options.name,
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                    }),
                )
            }
            "ping" => result_response(id, json!({})),
            "tools/list" | "tools/call" if !session.is_initialized() => {
                error_response(id, INVALID_REQUEST, "initialize first")
            }
            "tools/list" => result_response(id, json!({"tools": self.shared.wire})),
            _ => error_response(id, METHOD_NOT_FOUND, "method not found"),
        }
    }

    /// Runs one `tools/call`. `None` means the request was cancelled (by the client or by `stop`)
    /// and gets no answer.
    pub(crate) async fn call_tool(
        &self,
        session: &Session,
        id: &Value,
        params: Option<&Value>,
        stop: &CancellationToken,
    ) -> Option<Value> {
        if !session.is_initialized() {
            return Some(error_response(id, INVALID_REQUEST, "initialize first"));
        }
        let (name, arguments) = match parse_call(params) {
            Ok(parsed) => parsed,
            Err(message) => return Some(error_response(id, INVALID_PARAMS, message)),
        };
        if !self.shared.definitions.iter().any(|d| d.name == name) {
            return Some(error_response(
                id,
                INVALID_PARAMS,
                &format!("unknown tool: {name}"),
            ));
        }
        let Some(flight) = session.register(id) else {
            return Some(error_response(
                id,
                INVALID_REQUEST,
                "a request with this id is already running",
            ));
        };
        let call = ToolCall::new(
            format!("mcp-{id}"),
            name,
            map::arguments_text(arguments.as_ref()),
        );
        let cancelled = &flight.token;
        let permit = tokio::select! {
            () = cancelled.cancelled() => return None,
            () = stop.cancelled() => return None,
            permit = self.shared.permits.acquire() => match permit {
                Ok(permit) => permit,
                Err(_) => return Some(error_response(id, INTERNAL_ERROR, INTERNAL)),
            },
        };
        // The call runs in its own task so a panic cannot take the session down. Dropping the
        // guard aborts the task, which drops the call future: the real cancel the tool set
        // documents, also when the HTTP client disconnects.
        let tools = self.shared.tools.clone();
        let mut task = AbortOnDrop(tokio::spawn(
            async move { tools.call_detailed(&call).await },
        ));
        let joined = tokio::select! {
            () = cancelled.cancelled() => return None,
            () = stop.cancelled() => return None,
            joined = &mut task.0 => joined,
        };
        drop(permit);
        Some(match joined {
            Ok(report) => result_response(id, map::to_call_result(&report.result)),
            Err(_) => error_response(id, INTERNAL_ERROR, INTERNAL),
        })
    }

    /// Whether a cancellation of `id` was requested; used by the HTTP layer for the owed answer.
    pub(crate) fn cancelled_response(id: &Value) -> Value {
        error_response(id, CANCELLED, "request cancelled")
    }
}

type ParsedCall = (String, Option<Map<String, Value>>);

/// The tool name and arguments of `tools/call` params.
fn parse_call(params: Option<&Value>) -> Result<ParsedCall, &'static str> {
    let Some(Value::Object(params)) = params else {
        return Err("params must be an object");
    };
    let Some(Value::String(name)) = params.get("name") else {
        return Err("name is required");
    };
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => Some(map.clone()),
        Some(_) => return Err("arguments must be an object"),
    };
    Ok((name.clone(), arguments))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn kind(value: Value) -> &'static str {
        match classify(value) {
            Message::Request { .. } => "request",
            Message::Notification { .. } => "notification",
            Message::Response => "response",
            Message::Invalid(_) => "invalid",
        }
    }

    #[test]
    fn classifies_messages() {
        assert_eq!(
            kind(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})),
            "request"
        );
        assert_eq!(
            kind(json!({"jsonrpc": "2.0", "id": "a", "method": "ping"})),
            "request"
        );
        assert_eq!(
            kind(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
            "notification"
        );
        assert_eq!(
            kind(json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
            "response"
        );
        assert_eq!(
            kind(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 1, "message": "x"}})),
            "response"
        );
        for bad in [
            json!([1]),
            json!("x"),
            json!({"id": 1, "method": "ping"}),
            json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": null, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": [1], "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 1}),
            json!({"jsonrpc": "2.0", "id": 1, "method": 5}),
        ] {
            assert_eq!(kind(bad.clone()), "invalid", "{bad}");
        }
    }

    #[test]
    fn call_params_are_checked() {
        assert!(parse_call(None).is_err());
        assert!(parse_call(Some(&json!([]))).is_err());
        assert!(parse_call(Some(&json!({}))).is_err());
        assert!(parse_call(Some(&json!({"name": 5}))).is_err());
        assert!(parse_call(Some(&json!({"name": "x", "arguments": [1]}))).is_err());
        let (name, args) = parse_call(Some(&json!({"name": "x"}))).unwrap();
        assert_eq!((name.as_str(), args), ("x", None));
        let (_, args) = parse_call(Some(&json!({"name": "x", "arguments": {"a": 1}}))).unwrap();
        assert_eq!(args.unwrap()["a"], 1);
    }

    #[test]
    fn error_responses_carry_the_id() {
        let e = error_response(&json!("k"), INVALID_PARAMS, "m");
        assert_eq!(e["id"], "k");
        assert_eq!(e["error"]["code"], -32602);
        assert_eq!(e["jsonrpc"], "2.0");
    }
}
