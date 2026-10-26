// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Sample audit entries for tests and screenshots. Entries are built in memory; nothing is read
//! from or written to a real audit file.

#![allow(clippy::field_reassign_with_default)] // the audit types are `non_exhaustive`

use sqler_ai_tools::{
    ApprovalOutcome, AuditConnection, AuditEntry, AuditFailure, AuditStatement, CallOutcome,
    Surface, names,
};

/// The canary that tests plant in fields the viewer must never show.
pub const CANARY: &str = "CANARY-SECRET";

fn statement(sql: &str, approval: ApprovalOutcome, rows: u64) -> AuditStatement {
    let mut s = AuditStatement::default();
    sql.clone_into(&mut s.sql);
    let class = if sql.trim_start().to_lowercase().starts_with("select") {
        "read"
    } else {
        "write"
    };
    class.clone_into(&mut s.class);
    s.approval = approval;
    s.ran = true;
    s.rows = Some(rows);
    s
}

#[allow(clippy::too_many_arguments)] // a fixture row, spelled out
fn entry(
    n: u64,
    now_ms: u64,
    age_s: u64,
    surface: Surface,
    tool: &str,
    conn: (&str, &str),
    approval: ApprovalOutcome,
    statements: Vec<AuditStatement>,
) -> AuditEntry {
    let mut e = AuditEntry::default();
    e.id = format!("fixture-{n:04}");
    e.ts_ms = now_ms.saturating_sub(age_s * 1000);
    e.surface = surface;
    tool.clone_into(&mut e.tool);
    e.connection = Some(AuditConnection::new(conn.0, conn.1));
    e.rows = statements.iter().filter_map(|s| s.rows).sum();
    e.result_bytes = 400 + e.rows * 24;
    e.duration_ms = 12 + n * 7;
    e.approval = approval;
    e.statements = statements;
    e
}

/// Eight entries, newest first, covering every surface, the approval outcomes, an error, a
/// catalog tool without SQL, a masked literal, two statements and three connections.
#[must_use]
#[allow(clippy::too_many_lines)] // one literal row per case
pub fn entries(now_ms: u64) -> Vec<AuditEntry> {
    use ApprovalOutcome as A;
    let run = names::RUN_QUERY;
    let mut v = vec![
        entry(
            8,
            now_ms,
            45,
            Surface::Assist,
            run,
            ("c-dev", "dev"),
            A::NotNeeded,
            vec![statement(
                "select id, email from accounts order by id",
                A::NotNeeded,
                120,
            )],
        ),
        entry(
            7,
            now_ms,
            600,
            Surface::Session,
            run,
            ("c-staging", "staging"),
            A::Approved,
            vec![
                statement(
                    "create index idx_acct_email on acct (email);",
                    A::Approved,
                    0,
                ),
                statement("analyze acct", A::Approved, 0),
            ],
        ),
        entry(
            6,
            now_ms,
            3 * 3600,
            Surface::Mcp,
            run,
            ("c-prod", "prod"),
            A::Denied,
            vec![{
                let mut s = statement("delete from orders where status = 'old'", A::Denied, 0);
                s.ran = false;
                s.rows = None;
                s
            }],
        ),
        entry(
            5,
            now_ms,
            5 * 3600,
            Surface::Agent,
            run,
            ("c-prod", "prod"),
            A::Refused,
            vec![{
                let mut s = statement("drop table audit_scratch", A::Refused, 0);
                s.ran = false;
                s.rows = None;
                s
            }],
        ),
        entry(
            4,
            now_ms,
            26 * 3600,
            Surface::Chat,
            names::LIST_TABLES,
            ("c-dev", "dev"),
            A::NotNeeded,
            Vec::new(),
        ),
        entry(
            3,
            now_ms,
            2 * 86_400,
            Surface::Session,
            run,
            ("c-dev", "dev"),
            A::Approved,
            vec![statement("update acct set ssn = ?", A::Approved, 3)],
        ),
        entry(
            2,
            now_ms,
            3 * 86_400,
            Surface::Mcp,
            run,
            ("c-staging", "staging"),
            A::NotNeeded,
            vec![{
                let mut s = statement("select * from missing_table", A::NotNeeded, 0);
                s.failed = true;
                s.rows = None;
                s
            }],
        ),
        entry(
            1,
            now_ms,
            9 * 86_400,
            Surface::Agent,
            names::SAMPLE_ROWS,
            ("c-dev", "dev"),
            A::NotNeeded,
            vec![statement("select * from accounts limit 5", A::NotNeeded, 5)],
        ),
    ];
    // The failed call: an error with a code. Its message is a plant for the canary tests.
    if let Some(e) = v.iter_mut().find(|e| e.id == "fixture-0002") {
        e.outcome = CallOutcome::Error;
        e.error = Some(AuditFailure::new("sql_error", CANARY));
        e.rows = 0;
        e.result_bytes = 96;
    }
    // A non-`sql` argument carries the canary too; the viewer must never show it.
    if let Some(e) = v.iter_mut().find(|e| e.id == "fixture-0003") {
        e.arguments = serde_json::json!({ "note": CANARY });
    }
    v
}
