// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Property: no statement that is not a plain read reaches the driver without the capability, the approver and an
//! approval of exactly that text (at both stages on `prod`). A read never runs without `db.read`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;
use sqler_driver_api::sqler_core::Environment;
use sqler_plugin_api::Limits;
use sqler_sql::{Dialect, classify};

use super::exec::{QueryCtx, run_guarded};
use super::support::{Scripted, SqliteData, TestPlugin, caps, data};
use super::{ApprovalStage, Decision};
use crate::runtime::wit::query::QueryRequest;

/// Statements of every class, as the SQLite classifier sees them. Copied from `sqler-ai-tools/tests/guard_props.rs`.
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

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn nothing_but_reads_runs_without_capability_and_approval(
        picks in proptest::collection::vec(0..POOL.len(), 1..5),
        decisions in proptest::collection::vec(any::<bool>(), 1..12),
        read in any::<bool>(),
        write in any::<bool>(),
        prod in any::<bool>(),
        with_approver in any::<bool>(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let who = TestPlugin::new("props", "Props");
            let env = if prod { Environment::Prod } else { Environment::Dev };
            let src = SqliteData::new("prop", Some(env), false).await;
            let counter = Arc::new(AtomicUsize::new(0));
            let approver = {
                let counter = Arc::clone(&counter);
                let decisions = decisions.clone();
                Scripted::new(move |_| {
                    let i = counter.fetch_add(1, Ordering::SeqCst);
                    if decisions[i % decisions.len()] { Decision::Approve } else { Decision::Deny }
                })
            };
            let d = data(src.clone(), with_approver.then(|| approver.clone()));
            let mut names = Vec::new();
            if read { names.push("db.read"); }
            if write { names.push("db.write"); }
            let effective = caps(&names);
            for &i in &picks {
                let ctx = QueryCtx {
                    plugin: &who.plugin,
                    effective: &effective,
                    data: Some(Arc::clone(&d)),
                    limits: Limits::DEFAULT.query,
                };
                let request = QueryRequest {
                    connection: "db".to_owned(),
                    sql: POOL[i].to_owned(),
                    params: Vec::new(),
                    max_rows: None,
                    timeout_ms: None,
                };
                let _ = run_guarded(&ctx, request).await;
            }
            let requests = approver.requests();
            // Replays the scripted decisions to know what each request got, and counts the approvals per text. The
            // second stage is asked only after the first was approved, in the same run, so on `prod` the number of
            // fully approved runs of a text is its number of approved second stages.
            let mut confirmed: HashMap<String, usize> = HashMap::new();
            let mut second: HashMap<String, usize> = HashMap::new();
            for (i, r) in requests.iter().enumerate() {
                if decisions[i % decisions.len()] {
                    let map = match r.stage {
                        ApprovalStage::Confirm => &mut confirmed,
                        _ => &mut second,
                    };
                    *map.entry(r.sql.clone()).or_default() += 1;
                }
            }
            let mut executions: HashMap<String, usize> = HashMap::new();
            for text in src.executed() {
                *executions.entry(text).or_default() += 1;
            }
            if !with_approver {
                prop_assert!(requests.is_empty());
            }
            if !read && !write {
                prop_assert!(src.executed().is_empty(), "nothing runs without a db capability");
            }
            for (text, runs) in &executions {
                if classify(Dialect::Sqlite, text).is_read_only() {
                    prop_assert!(read, "{text} ran without db.read");
                    continue;
                }
                prop_assert!(write, "{text} ran without db.write");
                prop_assert!(with_approver, "{text} ran with no approver");
                let first = confirmed.get(text).copied().unwrap_or(0);
                prop_assert!(*runs <= first, "{text} ran {runs} times, approved {first}");
                if prod {
                    let both = second.get(text).copied().unwrap_or(0);
                    prop_assert!(
                        *runs <= both,
                        "{text} ran {runs} times, confirmed twice {both}"
                    );
                }
            }
            Ok(())
        })?;
    }
}
