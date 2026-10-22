// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::path::PathBuf;

use sqler_ai_tools::{AuditLog, AuditOptions, AuditStatement};

use super::fixture::{self, CANARY};
use super::*;

const NOW: u64 = 1_800_000_000_000;

fn temp_dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("sqler-audit-ui-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A log holding the fixture (appended oldest first, as the tools do).
fn log_with_fixture(dir: &std::path::Path) -> AuditLog {
    let log = AuditLog::open_at(dir.join("ai-audit.jsonl"), AuditOptions::default());
    for e in fixture::entries(NOW).iter().rev() {
        log.append(e).unwrap();
    }
    log
}

fn read(log: &AuditLog, p: &mut AuditPanel, more: bool) {
    let r = p.begin(more);
    let page = log.read(&r.query).unwrap();
    assert!(p.finish(r.request, Ok(page.into())));
}

fn ids(p: &AuditPanel) -> Vec<String> {
    p.entries().iter().map(|e| e.id.clone()).collect()
}

#[test]
fn the_fixture_covers_the_cases_and_is_newest_first() {
    let v = fixture::entries(NOW);
    assert_eq!(v.len(), 8);
    assert!(v.windows(2).all(|w| w[0].ts_ms > w[1].ts_ms));
    for s in AUDIT_SURFACES {
        assert!(v.iter().any(|e| e.surface == s), "{s:?}");
    }
    for d in [
        ApprovalOutcome::Approved,
        ApprovalOutcome::Denied,
        ApprovalOutcome::Refused,
        ApprovalOutcome::NotNeeded,
    ] {
        assert!(v.iter().any(|e| e.approval == d), "{d:?}");
    }
    assert!(v.iter().any(|e| e.outcome == CallOutcome::Error));
    assert!(v.iter().any(|e| e.statements.is_empty()));
    assert!(v.iter().any(|e| e.statements.len() == 2));
    for name in ["dev", "staging", "prod"] {
        assert!(
            v.iter()
                .any(|e| e.connection.as_ref().is_some_and(|c| c.name == name))
        );
    }
}

#[test]
fn reading_through_a_real_log_is_newest_first_and_filters_work() {
    let dir = temp_dir();
    let log = log_with_fixture(&dir);
    let mut p = AuditPanel::default();
    assert_eq!(*p.load(), AuditLoad::NotLoaded);
    read(&log, &mut p, false);
    assert_eq!(*p.load(), AuditLoad::Ready);
    assert_eq!(p.entries().len(), 8);
    assert!(p.entries().windows(2).all(|w| w[0].ts_ms > w[1].ts_ms));
    assert!(!p.has_more());

    assert!(p.set_filter(AuditFilterChange::Connection(Some("c-prod".into()))));
    read(&log, &mut p, false);
    assert_eq!(p.entries().len(), 2);
    assert!(p.set_filter(AuditFilterChange::Connection(None)));

    assert!(p.set_filter(AuditFilterChange::Surface(Some(Surface::Mcp))));
    read(&log, &mut p, false);
    assert_eq!(p.entries().len(), 2);
    assert!(p.entries().iter().all(|e| e.surface == Surface::Mcp));
    p.set_filter(AuditFilterChange::Surface(None));

    assert!(p.set_filter(AuditFilterChange::Tool(Some("list_tables".into()))));
    read(&log, &mut p, false);
    assert_eq!(p.entries().len(), 1);
    p.set_filter(AuditFilterChange::Tool(None));

    assert!(p.set_filter(AuditFilterChange::Decision(Some(ApprovalOutcome::Denied))));
    read(&log, &mut p, false);
    assert_eq!(p.entries().len(), 1);
    assert!(!p.set_filter(AuditFilterChange::Decision(Some(ApprovalOutcome::Denied))));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn paging_visits_every_entry_once() {
    let dir = temp_dir();
    let log = log_with_fixture(&dir);
    let mut p = AuditPanel::default();
    p.set_page_size(3);
    read(&log, &mut p, false);
    assert_eq!(p.entries().len(), 3);
    assert!(p.has_more());
    p.select(1);
    read(&log, &mut p, true);
    assert_eq!(p.selected(), Some(1), "more keeps the selection");
    assert_eq!(p.entries().len(), 6);
    read(&log, &mut p, true);
    assert_eq!(p.entries().len(), 8);
    assert!(!p.has_more());
    let mut all = ids(&p);
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 8);
    assert_eq!(ids(&p)[0], "fixture-0008");
    read(&log, &mut p, false);
    assert_eq!(p.entries().len(), 3);
    assert_eq!(p.selected(), None, "a first page clears the selection");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_torn_last_line_shows_in_the_notes() {
    use std::io::Write;
    let dir = temp_dir();
    let log = log_with_fixture(&dir);
    std::fs::OpenOptions::new()
        .append(true)
        .open(log.path())
        .unwrap()
        .write_all(b"{\"v\":1,\"id\":\"torn")
        .unwrap();
    let mut p = AuditPanel::default();
    read(&log, &mut p, false);
    assert_eq!(p.entries().len(), 8);
    assert_eq!(p.notes(), (1, 0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stale_answers_are_dropped_and_errors_keep_only_a_load_more_list() {
    let mut p = AuditPanel::default();
    let first = p.begin(false);
    let second = p.begin(false);
    assert!(!p.finish(first.request, Ok(AuditBatch::default())));
    assert_eq!(*p.load(), AuditLoad::Loading);
    let batch = AuditBatch {
        entries: fixture::entries(NOW),
        next: Some(sqler_ai_tools::AuditCursor::new(NOW, "fixture-0008")),
        ..AuditBatch::default()
    };
    assert!(p.finish(second.request, Ok(batch)));
    let more = p.begin(true);
    assert!(p.finish(more.request, Err("net".into())));
    assert_eq!(*p.load(), AuditLoad::Failed("net".into()));
    assert_eq!(p.entries().len(), 8, "a failed load-more keeps the list");
    let third = p.begin(false);
    assert!(p.finish(third.request, Err("disk".into())));
    assert_eq!(*p.load(), AuditLoad::Failed("disk".into()));
    assert!(
        p.entries().is_empty(),
        "a failed first page clears the list"
    );
    assert_eq!(format!("{first:?}"), "AuditRead { request: 1, .. }");
}

#[test]
fn copy_sql_is_disabled_after_a_failed_first_page() {
    let mut p = AuditPanel::default();
    let r = p.begin(false);
    let batch = AuditBatch {
        entries: fixture::entries(NOW),
        ..AuditBatch::default()
    };
    assert!(p.finish(r.request, Ok(batch)));
    p.select(0);
    assert!(p.selected_sql().is_some());
    let r = p.begin(false);
    assert!(p.finish(r.request, Err("disk".into())));
    assert_eq!(p.selected(), None);
    assert_eq!(p.selected_sql(), None);
}

#[test]
fn begin_maps_the_filters_to_the_query() {
    let mut p = AuditPanel::default();
    let q = p.begin(false).query;
    assert_eq!(q, AuditQuery::new().with_limit(AUDIT_PAGE));
    p.bind(Some("c1".into()));
    p.set_filter(AuditFilterChange::Surface(Some(Surface::Agent)));
    p.set_filter(AuditFilterChange::Tool(Some("run_query".into())));
    p.set_filter(AuditFilterChange::Decision(Some(ApprovalOutcome::Refused)));
    let q = p.begin(false).query;
    assert_eq!(q.connection.as_deref(), Some("c1"));
    assert_eq!(q.surface, Some(Surface::Agent));
    assert_eq!(q.tool.as_deref(), Some("run_query"));
    assert_eq!(q.approval, Some(ApprovalOutcome::Refused));
    assert!(q.cursor.is_none());
    assert_eq!(p.bound(), Some("c1"));
}

#[test]
fn rows_have_readable_labels_tones_and_results() {
    let v = fixture::entries(NOW);
    let r = audit_row(&v[0], NOW);
    assert_eq!(r.when, "just now");
    assert_eq!(r.surface, "In-app assistant");
    assert_eq!(r.connection, "dev");
    assert_eq!(r.decision, "Not needed");
    assert_eq!(r.tone, AuditTone::Muted);
    assert_eq!(r.result, "120 rows · 3.2 KiB");
    assert_eq!(r.more_statements, 0);

    let r = audit_row(&v[1], NOW);
    assert_eq!(r.when, "10 min ago");
    assert_eq!(r.tone, AuditTone::Success);
    assert_eq!(r.more_statements, 1);
    assert_eq!(r.surface, "In-app session");

    assert_eq!(audit_row(&v[2], NOW).tone, AuditTone::Danger);
    assert_eq!(audit_row(&v[2], NOW).when, "3 h ago");
    assert_eq!(audit_row(&v[3], NOW).tone, AuditTone::Danger);
    assert_eq!(audit_row(&v[3], NOW).surface, "ACP agent");
    assert_eq!(audit_row(&v[4], NOW).sql, "");
    assert_eq!(audit_row(&v[4], NOW).when, "1 d ago");
    assert_eq!(audit_row(&v[6], NOW).result, "Error: sql_error");
    assert_eq!(audit_row(&v[6], NOW).surface, "MCP");

    let mut none = AuditEntry::default();
    assert_eq!(audit_row(&none, NOW).connection, "—");
    assert_eq!(audit_row(&none, NOW).surface, "Unspecified");
    none.statements.push(AuditStatement::default());
    none.statements[0].sql = format!("select 1\nfrom t{}", "x".repeat(300));
    assert_eq!(audit_row(&none, NOW).sql, "select 1");
    none.statements[0].sql = "x".repeat(300);
    assert_eq!(audit_row(&none, NOW).sql.chars().count(), 200);
}

#[test]
fn unknown_values_render_gracefully() {
    assert_eq!(wire_label("cancelled"), "Cancelled");
    assert_eq!(wire_label("not_asked"), "Not asked");
    assert_eq!(wire_label(""), "");
}

#[test]
fn a_sql_argument_is_the_fallback() {
    let mut e = AuditEntry::default();
    e.arguments = serde_json::json!({ "sql": "select 1;  ", "other": CANARY });
    assert_eq!(audit_row(&e, NOW).sql, "select 1;  ");
    assert_eq!(entry_sql(&e).as_deref(), Some("select 1;  "));
    e.arguments = serde_json::json!({ "other": CANARY });
    assert_eq!(entry_sql(&e), None);
    assert_eq!(audit_row(&e, NOW).sql, "");
}

#[test]
fn entry_sql_joins_statements() {
    let mut e = AuditEntry::default();
    assert_eq!(entry_sql(&e), None);
    e.statements.push(AuditStatement::default());
    e.statements[0].sql = "select 1".into();
    assert_eq!(entry_sql(&e).as_deref(), Some("select 1;"));
    e.statements.push(AuditStatement::default());
    e.statements[1].sql = "select 2 ;\n".into();
    assert_eq!(entry_sql(&e).as_deref(), Some("select 1;\nselect 2;"));
}

#[test]
fn the_canary_never_reaches_the_viewer() {
    let mut entries = fixture::entries(NOW);
    // One entry that carries the canary everywhere the viewer must not look, plus masked SQL.
    let mut e = AuditEntry::default();
    e.id = "canary".into();
    e.tool = "run_query".into();
    e.arguments = serde_json::json!({ "note": CANARY, "params": [CANARY] });
    e.outcome = CallOutcome::Error;
    e.error = Some(sqler_ai_tools::AuditFailure::new("sql_error", CANARY));
    e.statements.push(AuditStatement::default());
    e.statements[0].sql = "update acct set ssn = ?".into();
    entries.push(e);

    let mut p = AuditPanel::default();
    p.set_fixture(entries.clone());
    for (ix, e) in entries.iter().enumerate() {
        let row = audit_row(e, NOW);
        let shown = format!(
            "{} {} {} {} {} {} {} {}",
            row.when,
            row.surface,
            row.connection,
            row.tool,
            row.sql,
            row.decision,
            row.result,
            row.more_statements
        );
        assert!(!shown.contains(CANARY), "{shown}");
        assert!(!format!("{row:?}").contains(CANARY));
        assert!(!entry_sql(e).unwrap_or_default().contains(CANARY));
        p.select(ix);
        assert!(!p.selected_sql().unwrap_or_default().contains(CANARY));
        assert!(!format!("{p:?}").contains(CANARY));
    }
    // The masked literal is shown and copied as stored (plus the closing `;` of a copy).
    let last = entries.len() - 1;
    assert_eq!(
        audit_row(&entries[last], NOW).sql,
        "update acct set ssn = ?"
    );
    p.select(last);
    assert_eq!(
        p.selected_sql().as_deref(),
        Some("update acct set ssn = ?;")
    );
    assert!(!format!("{p:?}").contains("ssn"));
    assert!(!format!("{:?}", audit_row(&entries[last], NOW)).contains("ssn"));
}

#[test]
fn tools_follow_the_schema() {
    let defs = sqler_ai_tools::definitions(true);
    assert_eq!(AUDIT_TOOLS.len(), 12);
    assert!(defs.iter().all(|d| AUDIT_TOOLS.contains(&d.name.as_str())));
}
