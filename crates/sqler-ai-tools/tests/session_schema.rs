// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The chat session sends a compact schema of the connection with every turn (F3).
#![allow(clippy::unwrap_used)]

mod common;
mod session_common;

use std::sync::{Arc, Mutex};

use common::{SEED, SqliteSource, limits};
use serde_json::Value;
use session_common::{TestProvider, events, provider, text};
use sqler_ai_api::mock::ScriptedProvider;
use sqler_ai_tools::session::{
    ExplorationSession, FocusTable, PromptKind, SchemaContextOptions, SessionCancel, SessionConfig,
    TurnOutcome, system_prompt,
};
use sqler_ai_tools::{
    ConnectionInfo, SessionSource, ToolSet, UNTRUSTED_CLOSE, UNTRUSTED_NOTICE, UNTRUSTED_OPEN,
    unwrap_untrusted,
};
use sqler_catalog::Catalog;

const ASK: &str = "write an INSERT into orders for user 1";

fn chat_config(options: Option<SchemaContextOptions>) -> SessionConfig {
    let config = SessionConfig::new("m").with_prompt(PromptKind::Chat {
        max_rows: 50,
        timeout_secs: 20,
    });
    match options {
        Some(o) => config.with_schema_context(o),
        None => config,
    }
}

fn chat(
    source: &Arc<SqliteSource>,
    p: &Arc<ScriptedProvider>,
    options: Option<SchemaContextOptions>,
) -> ExplorationSession {
    let tools = ToolSet::new(source.clone(), limits());
    ExplorationSession::new(
        p.clone(),
        tools,
        source.info.clone(),
        None,
        chat_config(options),
    )
    .unwrap()
}

/// The JSON of the schema block in a system prompt.
fn payload(system: &str) -> Value {
    let from = system.find(UNTRUSTED_NOTICE).unwrap();
    serde_json::from_str(unwrap_untrusted(&system[from..]).unwrap()).unwrap()
}

fn tables(system: &str) -> Vec<String> {
    payload(system)["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_owned())
        .collect()
}

async fn system_of(
    source: &Arc<SqliteSource>,
    options: SchemaContextOptions,
    focus: Vec<FocusTable>,
    text_in: &str,
) -> String {
    let p = provider();
    p.push_script(text("ok"));
    let mut s = chat(source, &p, Some(options));
    s.set_focus(focus);
    assert_eq!(s.send(text_in).await, TurnOutcome::Completed);
    p.requests()[0].system.clone().unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn chat_request_carries_schema_context() {
    let source = SqliteSource::new("schema-ai1", None).await;
    let p = provider();
    p.push_script(text("ok"));
    let mut s = chat(&source, &p, Some(SchemaContextOptions::default()));
    assert_eq!(s.send(ASK).await, TurnOutcome::Completed);
    let system = p.requests()[0].system.clone().unwrap();
    assert!(system.contains("orders("), "{system}");
    // The envelope writes `>` as `\u003e`; the parsed lines carry the plain arrow.
    let lines = tables(&system).join("\n");
    assert!(
        lines
            .to_lowercase()
            .contains("user_id integer -> main.users.id"),
        "{lines}"
    );
    assert!(system.contains("Use only tables and columns that exist"));
    // Only fixture tables are named, each once.
    assert_eq!(tables(&system).len(), 3, "{lines}");
    for name in ["users", "orders", "user_names"] {
        assert!(lines.contains(name), "{lines}");
    }
    assert!(!lines.contains("invoices"), "{lines}");
    assert_eq!(
        s.schema_context().map(|c| c.contains("orders(")),
        Some(true)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_focus_table_comes_first_even_when_the_prompt_names_another() {
    let source = SqliteSource::new("schema-focus", None).await;
    let focus = vec![FocusTable::new(None, None, "users")];
    let system = system_of(&source, SchemaContextOptions::default(), focus, ASK).await;
    let lines = tables(&system);
    assert!(lines[0].contains("users("), "{lines:?}");
    assert!(lines[1].contains("orders("), "{lines:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_table_named_in_the_prompt_comes_before_an_unnamed_one() {
    let mut seed = SEED.to_vec();
    seed.push("create table audit_log (id integer primary key, note text)");
    let source = SqliteSource::seeded("schema-named", None, None, &seed).await;
    let opts = SchemaContextOptions::default();
    // Unnamed: the listing order (by name) puts `audit_log` first.
    let plain = tables(&system_of(&source, opts, vec![], "hello").await);
    assert!(plain[0].contains("audit_log("), "{plain:?}");
    let named = tables(&system_of(&source, opts, vec![], ASK).await);
    assert!(named[0].contains("orders("), "{named:?}");
    let audit = named.iter().position(|l| l.contains("audit_log(")).unwrap();
    assert!(audit > 0, "{named:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_small_budget_truncates_and_is_never_exceeded() {
    let source = SqliteSource::new("schema-budget", None).await;
    let mut opts = SchemaContextOptions::default();
    opts.token_budget = 40;
    let system = system_of(&source, opts, vec![], ASK).await;
    let from = system.find(UNTRUSTED_NOTICE).unwrap();
    let json = unwrap_untrusted(&system[from..]).unwrap();
    assert!(json.len() <= 120, "{} bytes: {json}", json.len());
    assert_eq!(payload(&system)["truncated"], true);

    // The default budget fits this small schema whole.
    let system = system_of(&source, SchemaContextOptions::default(), vec![], ASK).await;
    assert_eq!(payload(&system)["truncated"], false);
    let from = system.find(UNTRUSTED_NOTICE).unwrap();
    assert!(unwrap_untrusted(&system[from..]).unwrap().len() <= 1024 * 3);
}

#[tokio::test(flavor = "current_thread")]
async fn tables_beyond_max_described_are_listed_by_name() {
    let source = SqliteSource::new("schema-others", None).await;
    let mut opts = SchemaContextOptions::default();
    opts.max_described = 1;
    let system = system_of(&source, opts, vec![], ASK).await;
    let p = payload(&system);
    assert_eq!(p["tables"].as_array().unwrap().len(), 1);
    let others = p["other_tables"].to_string();
    assert!(
        others.contains("users") && others.contains("user_names"),
        "{others}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn redacted_columns_stay_out_and_the_block_is_enveloped() {
    let seed = ["create table accounts (id integer primary key, \
         loyalty_no text default 'CANARY-DEF', note text)"];
    let source = SqliteSource::seeded("schema-redact", None, None, &seed).await;
    let list = sqler_config::RedactionList::new(["loyalty_no".to_owned()]).unwrap();
    let tools = ToolSet::new(source.clone(), limits()).with_redaction(list);
    let tools_for_check = tools.clone();
    let p = provider();
    p.push_script(text("ok"));
    let mut s = ExplorationSession::new(
        p.clone(),
        tools,
        source.info.clone(),
        None,
        chat_config(Some(SchemaContextOptions::default())),
    )
    .unwrap();
    assert_eq!(s.send("accounts").await, TurnOutcome::Completed);
    let system = p.requests()[0].system.clone().unwrap();
    // The renderer drops column defaults, so prove the redaction on the body it reads: the
    // plain tool set returns the canary, the redacted one does not.
    let describe = sqler_ai_api::ToolCall::new(
        "1",
        "describe_table",
        r#"{"connection":"db","name":"accounts"}"#,
    );
    let plain = ToolSet::new(source.clone(), limits()).call(&describe).await;
    assert!(plain.content.contains("CANARY-DEF"), "{}", plain.content);
    let hidden = tools_for_check.call(&describe).await;
    assert!(!hidden.content.contains("CANARY"), "{}", hidden.content);
    assert!(!system.contains("CANARY"), "{system}");
    let notice = system.find(UNTRUSTED_NOTICE).unwrap();
    let open = system[notice..].find(UNTRUSTED_OPEN).unwrap() + notice;
    assert!(notice < open);
    assert!(system.ends_with(UNTRUSTED_CLOSE), "{system}");
    assert!(tables(&system)[0].contains("accounts("));
}

#[tokio::test(flavor = "current_thread")]
async fn a_tab_session_gets_exactly_the_tab_prompt() {
    let source = SqliteSource::new("schema-tab", None).await;
    let p = provider();
    p.push_script(text("ok"));
    let tools = ToolSet::new(source.clone(), limits());
    let mut s = ExplorationSession::new(
        p.clone(),
        tools,
        source.info.clone(),
        None,
        SessionConfig::new("m"),
    )
    .unwrap();
    assert_eq!(s.send(ASK).await, TurnOutcome::Completed);
    assert_eq!(
        p.requests()[0].system.as_deref(),
        Some(system_prompt(&source.info).as_str())
    );
    assert_eq!(s.schema_context(), None);
}

#[tokio::test(flavor = "current_thread")]
async fn building_the_context_runs_no_statement() {
    let source = SqliteSource::new("schema-ro", None).await;
    let _ = system_of(&source, SchemaContextOptions::default(), vec![], ASK).await;
    assert!(source.executed().is_empty(), "{:?}", source.executed());
}

/// A source whose catalog lookup cancels the session.
struct CancelSource {
    inner: Arc<SqliteSource>,
    cancel: Mutex<Option<SessionCancel>>,
}

impl SessionSource for CancelSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        self.inner.connections()
    }

    fn catalog(&self, connection: &str) -> Option<Catalog> {
        if let Some(c) = self.cancel.lock().unwrap().as_ref() {
            c.cancel();
        }
        self.inner.catalog(connection)
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> sqler_ai_api::BoxFuture<
        'a,
        Result<Box<dyn sqler_driver_api::Session>, sqler_ai_tools::ToolError>,
    > {
        self.inner.acquire(connection)
    }

    fn release(
        &self,
        connection: &str,
        session: Box<dyn sqler_driver_api::Session>,
    ) -> sqler_ai_api::BoxFuture<'static, ()> {
        self.inner.release(connection, session)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_during_the_build_ends_the_turn_cancelled() {
    let base = SqliteSource::new("schema-cancel", None).await;
    let source = Arc::new(CancelSource {
        inner: Arc::clone(&base),
        cancel: Mutex::new(None),
    });
    let p = TestProvider::new(vec![events(text("never"))]);
    let tools = ToolSet::new(source.clone(), limits());
    let mut s = ExplorationSession::new(
        p.clone(),
        tools,
        base.info.clone(),
        None,
        chat_config(Some(SchemaContextOptions::default())),
    )
    .unwrap();
    *source.cancel.lock().unwrap() = Some(s.cancel_handle());
    assert_eq!(s.send(ASK).await, TurnOutcome::Cancelled);
    assert_eq!(p.requests().len(), 0);
}
