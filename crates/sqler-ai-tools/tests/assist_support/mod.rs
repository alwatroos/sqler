// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Fixtures of the assist tests: a synthetic 5 000-table catalog, scripted and stalling
//! providers, and prompt helpers. Uses `tests/common` read-only.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqler_ai_api::mock::ScriptedProvider;
use sqler_ai_api::{
    AiError, AiProvider, BoxFuture, Capabilities, ChatEvent, ChatRequest, ChatStream, ModelInfo,
    Part, StopReason, ToolCall,
};
use sqler_ai_tools::{ConnectionInfo, SessionSource, ToolError, UNTRUSTED_CLOSE, UNTRUSTED_OPEN};
use sqler_catalog::Catalog;
use sqler_driver_api::sqler_core::{
    ColumnInfo, DataType, DbObject, DriverId, Listing, ObjectKind, ObjectPath, PrimaryKey, Table,
};
use sqler_driver_api::{DriverError, Introspect, Session};

/// The number of generated tables `t0000..t4999`.
pub const GENERATED: usize = 5_000;

/// A fake introspection: `customers`, `orders`, 5 000 generated tables and extra tables.
#[derive(Clone)]
pub struct Synthetic {
    tables: Arc<BTreeMap<String, Vec<ColumnInfo>>>,
}

impl Synthetic {
    pub fn new(extra: Vec<(&str, Vec<ColumnInfo>)>) -> Self {
        let int = |n: &str| ColumnInfo::new(n, DataType::Integer);
        let text = |n: &str| ColumnInfo::new(n, DataType::Text);
        let mut tables = BTreeMap::new();
        tables.insert(
            "customers".to_owned(),
            vec![int("id"), text("name"), text("email")],
        );
        tables.insert(
            "orders".to_owned(),
            vec![
                int("id"),
                int("customer_id"),
                ColumnInfo::new("total", DataType::Float),
            ],
        );
        for i in 0..GENERATED {
            tables.insert(format!("t{i:04}"), vec![int("id"), text("label")]);
        }
        for (name, columns) in extra {
            tables.insert(name.to_owned(), columns);
        }
        Self {
            tables: Arc::new(tables),
        }
    }
}

impl Introspect for Synthetic {
    fn supports(&self, kind: ObjectKind) -> bool {
        kind == ObjectKind::Table
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        let listing = if kind == ObjectKind::Table {
            Listing::Objects(
                self.tables
                    .keys()
                    .map(|n| DbObject::new(ObjectKind::Table, parent.clone(), n.as_str()))
                    .collect(),
            )
        } else {
            Listing::Unsupported
        };
        Box::pin(std::future::ready(Ok(listing)))
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        let found = self.tables.get(&table.name).map(|columns| {
            Table::new(table.clone())
                .with_columns(columns.clone())
                .with_primary_key(PrimaryKey::new(vec!["id".to_owned()]))
        });
        Box::pin(std::future::ready(Ok(found)))
    }

    fn definition<'a>(
        &'a mut self,
        _object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

/// A source over [`Synthetic`]. It lends no session: anything that tries to run SQL fails.
pub struct SyntheticSource {
    pub info: ConnectionInfo,
    pub catalog: Catalog,
    pub acquires: AtomicUsize,
}

impl SyntheticSource {
    pub fn new() -> Arc<Self> {
        Self::with(Vec::new())
    }

    pub fn with(extra: Vec<(&str, Vec<ColumnInfo>)>) -> Arc<Self> {
        let boxed: Box<dyn Introspect> = Box::new(Synthetic::new(extra));
        let (catalog, worker) = Catalog::new(boxed);
        tokio::spawn(worker.run());
        Arc::new(Self {
            info: ConnectionInfo::new("syn", "Synthetic", DriverId::new("sqlite").unwrap(), None),
            catalog,
            acquires: AtomicUsize::new(0),
        })
    }
}

impl SessionSource for SyntheticSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![self.info.clone()]
    }

    fn catalog(&self, _connection: &str) -> Option<Catalog> {
        Some(self.catalog.clone())
    }

    fn acquire<'a>(
        &'a self,
        _connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        self.acquires.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Err(ToolError::NotAvailable(
            "the synthetic source lends no session".to_owned(),
        ))))
    }
}

/// A text answer as provider events.
pub fn answer(text: &str) -> Vec<Result<ChatEvent, AiError>> {
    vec![
        Ok(ChatEvent::TextDelta(text.to_owned())),
        Ok(ChatEvent::Done {
            stop: StopReason::EndTurn,
        }),
    ]
}

/// A request for one tool call as provider events.
pub fn tool_call(id: &str, name: &str, args: &str) -> Vec<Result<ChatEvent, AiError>> {
    vec![
        Ok(ChatEvent::ToolCall(ToolCall::new(id, name, args))),
        Ok(ChatEvent::Done {
            stop: StopReason::ToolUse,
        }),
    ]
}

/// A scripted provider with the given capabilities and one script per answer.
pub fn scripted(
    tools: bool,
    json_mode: bool,
    scripts: Vec<Vec<Result<ChatEvent, AiError>>>,
) -> Arc<ScriptedProvider> {
    let provider = ScriptedProvider::new("mock")
        .with_capabilities(Capabilities::new(tools, json_mode, false, None));
    for script in scripts {
        provider.push_script(script);
    }
    Arc::new(provider)
}

/// A provider whose stream never produces an event. `open` counts the live streams.
#[derive(Default)]
pub struct StallProvider {
    pub open: Arc<AtomicUsize>,
}

impl StallProvider {
    pub fn open_streams(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }
}

struct StallStream(Arc<AtomicUsize>);

impl ChatStream for StallStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>> {
        Box::pin(std::future::pending())
    }
}

impl Drop for StallStream {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AiProvider for StallProvider {
    fn id(&self) -> &'static str {
        "stall"
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
        Box::pin(std::future::ready(Ok(Vec::new())))
    }

    fn stream(&self, _request: ChatRequest) -> Box<dyn ChatStream + '_> {
        self.open.fetch_add(1, Ordering::SeqCst);
        Box::new(StallStream(Arc::clone(&self.open)))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
}

/// A request as text: system prompt, every message part and the tool names.
pub fn render(request: &ChatRequest) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "model: {}", request.model);
    let _ = writeln!(out, "format: {:?}", request.response_format);
    let _ = writeln!(out, "max_output_tokens: {:?}", request.max_output_tokens);
    let tools: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
    let _ = writeln!(out, "tools: {tools:?}");
    out.push_str("=== system ===\n");
    out.push_str(request.system.as_deref().unwrap_or_default());
    out.push('\n');
    for (i, message) in request.messages.iter().enumerate() {
        let _ = writeln!(out, "=== message {i} ({:?}) ===", message.role());
        for part in message.parts() {
            match part {
                Part::Text(t) => out.push_str(t),
                Part::ToolCall(c) => {
                    let _ = write!(out, "[call {} {}]", c.name, c.arguments);
                }
                Part::ToolResult(r) => out.push_str(&r.content),
                _ => out.push_str("[other part]"),
            }
            out.push('\n');
        }
    }
    out
}

/// Everything the provider was sent, as one text.
pub fn all_requests(provider: &ScriptedProvider) -> String {
    provider
        .requests()
        .iter()
        .map(render)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Asserts that `needle` occurs in `prompt` and that every occurrence lies between an untrusted
/// open tag and its close tag.
pub fn assert_inside_envelopes(prompt: &str, needle: &str) {
    let mut found = 0;
    for (at, _) in prompt.match_indices(needle) {
        found += 1;
        let before = &prompt[..at];
        let open = before.rfind(UNTRUSTED_OPEN);
        let close = before.rfind(UNTRUSTED_CLOSE);
        assert!(
            open.is_some() && open > close,
            "`{needle}` at {at} is outside an envelope"
        );
        assert!(
            prompt[at..].contains(UNTRUSTED_CLOSE),
            "`{needle}` at {at} has no closing tag"
        );
    }
    assert!(found > 0, "`{needle}` is not in the prompt");
}
