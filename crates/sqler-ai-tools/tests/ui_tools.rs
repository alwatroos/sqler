// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The UI-only tools never hand a statement that needs approval to the host, and every tool
//! that carries SQL is accounted for.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use common::{ScriptedApprover, SqliteSource, call, limits};
use serde_json::{Value, json};
use sqler_ai_api::BoxFuture;
use sqler_ai_tools::{Decision, ToolError, ToolSet, UiHost, names};

/// Records what reaches the host.
#[derive(Default)]
struct Recorder {
    editor: Mutex<Vec<String>>,
    tabs: Mutex<Vec<String>>,
}

impl UiHost for Recorder {
    fn open_in_editor<'a>(
        &'a self,
        _: Option<&'a str>,
        sql: &'a str,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        self.editor.lock().unwrap().push(sql.to_owned());
        Box::pin(std::future::ready(Ok(())))
    }

    fn open_results_tab<'a>(
        &'a self,
        _: &'a str,
        sql: &'a str,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        self.tabs.lock().unwrap().push(sql.to_owned());
        Box::pin(std::future::ready(Ok(())))
    }
}

const NEEDS_APPROVAL: &[&str] = &[
    "EXPLAIN ANALYZE select 1",
    "explain analyse select 1",
    "EXPLAIN (ANALYZE, BUFFERS) select 1",
    "EXPLAIN/**/ANALYZE select 1",
    "/* a */ EXPLAIN -- b\n ANALYZE select 1",
    "select 1; EXPLAIN ANALYZE select 1",
    "delete from t",
    "select 1; drop table t",
];

#[tokio::test(flavor = "current_thread")]
async fn open_results_tab_refuses_everything_that_needs_approval() {
    for sql in NEEDS_APPROVAL {
        for approver in [None, Some(Decision::Deny), Some(Decision::Approve)] {
            let source = SqliteSource::new("ui-tab", None).await;
            let ui = Arc::new(Recorder::default());
            let mut ts = ToolSet::new(source.clone(), limits()).with_ui_host(ui.clone());
            if let Some(decision) = approver {
                ts = ts.with_approver(ScriptedApprover::always(decision));
            }
            let args = json!({"connection": source.info.id, "sql": sql});
            let (_, v) = call(&ts, names::OPEN_RESULTS_TAB, args).await;
            assert_eq!(v["error"]["code"], "invalid_arguments", "{sql:?} {v}");
            assert!(
                ui.tabs.lock().unwrap().is_empty(),
                "{sql:?} reached the host"
            );
            assert!(source.executed().is_empty(), "{sql:?}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn open_results_tab_still_opens_a_plain_read() {
    let source = SqliteSource::new("ui-ok", None).await;
    let ui = Arc::new(Recorder::default());
    let ts = ToolSet::new(source.clone(), limits()).with_ui_host(ui.clone());
    let args = json!({"connection": source.info.id, "sql": "select 1"});
    let (_, v) = call(&ts, names::OPEN_RESULTS_TAB, args).await;
    assert_eq!(v["opened"], true, "{v}");
    assert_eq!(ui.tabs.lock().unwrap().as_slice(), ["select 1"]);
}

/// How a tool that carries SQL treats a statement that needs approval.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stance {
    /// Goes through the approval gate, then the driver.
    Gated,
    /// Refuses such a statement outright.
    Refuses,
    /// Only puts text in an editor; nothing runs until the user runs it.
    TextOnly,
}

/// Every tool whose arguments carry SQL text must be listed here with its stance; a new tool
/// with a `sql` argument fails this test until someone decides how it is guarded.
#[tokio::test(flavor = "current_thread")]
async fn every_sql_tool_is_accounted_for() {
    let source = SqliteSource::new("ui-enum", None).await;
    let ui = Arc::new(Recorder::default());
    let ts = ToolSet::new(source.clone(), limits()).with_ui_host(ui.clone());
    let carrying: BTreeSet<String> = ts
        .definitions()
        .iter()
        .filter(|d| d.parameters["properties"].get("sql").is_some())
        .map(|d| d.name.clone())
        .collect();
    let known = [
        (names::RUN_QUERY, Stance::Gated),
        (names::EXPLAIN_QUERY, Stance::Gated),
        (names::OPEN_RESULTS_TAB, Stance::Refuses),
        (names::OPEN_IN_EDITOR, Stance::TextOnly),
    ];
    let expected: BTreeSet<String> = known.iter().map(|(n, _)| (*n).to_owned()).collect();
    assert_eq!(carrying, expected, "a tool with SQL is not classified");

    for (name, stance) in known {
        let sql = "EXPLAIN ANALYZE select 1";
        let mut args = json!({"connection": source.info.id, "sql": sql});
        if name == names::EXPLAIN_QUERY {
            args = json!({"connection": source.info.id, "sql": "select 1", "analyze": true});
        }
        // No approver: only a text-only tool may succeed, and only because nothing runs.
        let bare = ToolSet::new(source.clone(), limits()).with_ui_host(ui.clone());
        let (_, v): (_, Value) = call(&bare, name, args.clone()).await;
        match stance {
            // (SQLite refuses `analyze` itself; either way it is an error and nothing runs.)
            Stance::Gated | Stance::Refuses => assert!(v["error"].is_object(), "{name} {v}"),
            Stance::TextOnly => assert_eq!(v["opened"], true, "{name} {v}"),
        }
    }
    assert!(source.executed().is_empty(), "nothing reached the driver");
    assert!(ui.tabs.lock().unwrap().is_empty());
    assert_eq!(ui.editor.lock().unwrap().len(), 1);
}
