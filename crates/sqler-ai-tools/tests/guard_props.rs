// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Property: no statement that is not a plain read reaches the driver without an approval
//! (and on `prod`, without both approvals).
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{ScriptedApprover, SqliteSource, call, limits};
use proptest::prelude::*;
use serde_json::json;
use sqler_ai_tools::{ApprovalStage, Decision, ToolSet, names};
use sqler_driver_api::sqler_core::Environment;
use sqler_sql::{Dialect, classify};

/// Statements of every class, as the SQLite classifier sees them.
const POOL: &[&str] = &[
    "select id, name from users",
    "select count(*) from orders",
    "insert into users (id, name) values (100, 'p')",
    "update users set name = 'q' where id = 1",
    "delete from orders",
    "create table zz (a integer)",
    "drop table if exists zz",
    "pragma user_version = 2",
    "begin",
    "frobnicate everything",
    "vacuum",
    "explain query plan delete from users",
];

fn statements() -> impl Strategy<Value = Vec<usize>> {
    proptest::collection::vec(0..POOL.len(), 1..5)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn nothing_but_reads_runs_without_approval(
        picks in statements(),
        decisions in proptest::collection::vec(any::<bool>(), 1..12),
        prod in any::<bool>(),
        with_approver in any::<bool>(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let env = if prod { Some(Environment::Prod) } else { Some(Environment::Dev) };
            let source = SqliteSource::new("prop", env).await;
            let counter = Arc::new(AtomicUsize::new(0));
            let approver = {
                let counter = Arc::clone(&counter);
                let decisions = decisions.clone();
                ScriptedApprover::new(move |_| {
                    let i = counter.fetch_add(1, Ordering::SeqCst);
                    if decisions[i % decisions.len()] { Decision::Approve } else { Decision::Deny }
                })
            };
            let mut ts = ToolSet::new(source.clone(), limits());
            if with_approver {
                ts = ts.with_approver(approver.clone());
            }
            let sql = picks.iter().map(|&i| POOL[i]).collect::<Vec<_>>().join("; ");
            let (report, _) = call(&ts, names::RUN_QUERY, json!({"connection": "db", "sql": sql})).await;

            let requests = approver.requests();
            let mut confirmed = HashSet::new();
            let mut second = HashSet::new();
            let mut all_approved = true;
            // Replays the scripted decisions to know what each request got.
            for (i, r) in requests.iter().enumerate() {
                let approved = decisions[i % decisions.len()];
                all_approved &= approved;
                if approved {
                    match r.stage {
                        ApprovalStage::Confirm => confirmed.insert(r.sql.clone()),
                        _ => second.insert(r.sql.clone()),
                    };
                }
            }
            if !with_approver {
                prop_assert!(requests.is_empty());
            }
            let executed = source.executed();
            for text in &executed {
                if classify(Dialect::Sqlite, text).is_read_only() {
                    continue;
                }
                prop_assert!(with_approver, "{text} ran with no approver");
                prop_assert!(confirmed.contains(text), "{text} ran without approval");
                if prod {
                    prop_assert!(second.contains(text), "{text} ran without the prod confirmation");
                }
            }
            if !executed.is_empty() && picks.iter().any(|&i| !classify(Dialect::Sqlite, POOL[i]).is_read_only()) {
                // Any non-read in the call means every approval asked was a yes.
                prop_assert!(all_approved);
            }
            if report.result.is_error {
                prop_assert!(executed.is_empty(), "an error result must mean nothing ran");
            }
            Ok(())
        })?;
    }
}
