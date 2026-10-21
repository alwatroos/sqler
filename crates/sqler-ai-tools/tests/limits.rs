// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Row and time limits, on prod and off.
#![allow(clippy::unwrap_used)]

mod common;

use std::time::{Duration, Instant};

use common::{ScriptedApprover, SqliteSource, call};
use serde_json::json;
use sqler_ai_tools::{Decision, Limits, ToolSet, Truncation, names};
use sqler_driver_api::sqler_core::Environment;

const ALL_USERS: &str = "select id from users order by id";

fn query(sql: &str) -> serde_json::Value {
    json!({"connection": "db", "sql": sql})
}

#[tokio::test(flavor = "current_thread")]
async fn row_cap_truncates_and_says_so() {
    let source = SqliteSource::new("rows", None).await;
    let ts = ToolSet::new(source, Limits::new(3, Duration::from_secs(30), false));
    let (report, v) = call(&ts, names::RUN_QUERY, query(ALL_USERS)).await;
    let st = &v["statements"][0];
    assert_eq!(st["row_count"], 3);
    assert_eq!(st["rows"], json!([[1], [2], [3]]));
    assert_eq!(st["truncated"], "row_limit");
    assert_eq!(st["notice"], "Truncated at 3 rows (row limit).");
    assert_eq!(report.executed[0].truncated, Some(Truncation::RowLimit));
    assert_eq!(report.executed[0].rows, 3);
    assert_eq!(v["limits"]["max_rows"], 3);

    // The caller may ask for fewer rows, never for more.
    let (_, v) = call(
        &ts,
        names::RUN_QUERY,
        json!({"connection": "db", "sql": ALL_USERS, "max_rows": 2}),
    )
    .await;
    assert_eq!(v["statements"][0]["row_count"], 2);
    assert_eq!(
        v["statements"][0]["notice"],
        "Truncated at 2 rows (row limit)."
    );
    let (_, v) = call(
        &ts,
        names::RUN_QUERY,
        json!({"connection": "db", "sql": ALL_USERS, "max_rows": 50}),
    )
    .await;
    assert_eq!(v["statements"][0]["row_count"], 3);

    // Exactly at the cap is not truncated.
    let (_, v) = call(
        &ts,
        names::RUN_QUERY,
        query("select id from users where id <= 3"),
    )
    .await;
    assert_eq!(v["statements"][0]["row_count"], 3);
    assert_eq!(v["statements"][0]["truncated"], json!(null));

    // The cap is per statement, and the call goes on after a truncated one.
    let (_, v) = call(
        &ts,
        names::RUN_QUERY,
        query("select id from users; select id from users"),
    )
    .await;
    assert_eq!(v["statements"][0]["truncated"], "row_limit");
    assert_eq!(v["statements"][1]["truncated"], "row_limit");
    assert_eq!(v["not_run"], 0);

    // sample_rows is capped too.
    let (_, v) = call(
        &ts,
        names::SAMPLE_ROWS,
        json!({"connection": "db", "table": "users", "limit": 100}),
    )
    .await;
    assert_eq!(v["statements"][0]["row_count"], 3);
}

#[tokio::test(flavor = "current_thread")]
async fn prod_ignores_unlimited() {
    let limits = Limits::new(3, Duration::from_secs(30), true);

    let dev = SqliteSource::new("unl-dev", Some(Environment::Dev)).await;
    let ts = ToolSet::new(dev, limits);
    let (_, v) = call(&ts, names::RUN_QUERY, query(ALL_USERS)).await;
    assert_eq!(v["statements"][0]["row_count"], 5);
    assert_eq!(v["limits"], json!({"max_rows": null, "timeout_ms": null}));

    let prod = SqliteSource::new("unl-prod", Some(Environment::Prod)).await;
    let ts = ToolSet::new(prod, limits);
    let (_, v) = call(&ts, names::RUN_QUERY, query(ALL_USERS)).await;
    assert_eq!(v["statements"][0]["row_count"], 3);
    assert_eq!(v["statements"][0]["truncated"], "row_limit");
    assert_eq!(v["limits"], json!({"max_rows": 3, "timeout_ms": 30000}));
}

#[tokio::test(flavor = "current_thread")]
async fn timeout_cancels_and_the_database_stays_usable() {
    let source = SqliteSource::new("time", None).await;
    let limits = Limits::new(2_000_000_000, Duration::from_millis(200), false);
    // The classifier cannot prove `c(x)` is not a function call, so the CTE needs an approval.
    let ts = ToolSet::new(source.clone(), limits)
        .with_approver(ScriptedApprover::always(Decision::Approve));
    let started = Instant::now();
    let (report, v) = call(
        &ts,
        names::RUN_QUERY,
        query(
            "with recursive c(x) as (select 1 union all select x + 1 from c) \
             select x from c; select 1",
        ),
    )
    .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!report.result.is_error, "{v}");
    let st = &v["statements"][0];
    assert_eq!(st["truncated"], "time_limit");
    assert_eq!(
        st["notice"],
        "Stopped after 200 ms (time limit); the query was cancelled."
    );
    assert_eq!(v["not_run"], 1, "the second statement never started");
    assert_eq!(report.executed[0].truncated, Some(Truncation::TimeLimit));
    assert_eq!(source.executed().len(), 1);

    // The session was released, and a new one works.
    let (report, v) = call(&ts, names::RUN_QUERY, query("select count(*) from users")).await;
    assert!(!report.result.is_error);
    assert_eq!(v["statements"][0]["rows"], json!([[5]]));
    assert_eq!(source.acquire_count(), 2);
    assert_eq!(source.releases.load(std::sync::atomic::Ordering::SeqCst), 2);
}
