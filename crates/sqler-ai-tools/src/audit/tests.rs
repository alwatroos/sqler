// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::time::{Duration, SystemTime};

use serde_json::json;
use sqler_sql::StatementClass;

use super::draft::{AuditDraft, DraftError, DraftRun, DraftStatement};
use super::wire::serialize_line;
use super::*;
use sqler_sql::Dialect;

use crate::redact::{Redaction, RedactionSummary};

fn st(sql: &str, needs: bool, ran: bool) -> DraftStatement<'_> {
    DraftStatement {
        sql,
        class: if needs {
            StatementClass::Write
        } else {
            StatementClass::Read
        },
        needs_approval: needs,
        ran: ran.then_some(DraftRun {
            rows: 2,
            truncated: None,
            failed: false,
        }),
    }
}

fn draft<'a>(
    raw: &'a str,
    statements: Vec<DraftStatement<'a>>,
    error: Option<DraftError<'a>>,
) -> AuditDraft<'a> {
    AuditDraft {
        started: SystemTime::UNIX_EPOCH + Duration::from_millis(1234),
        duration: Duration::from_millis(7),
        surface: Surface::Mcp,
        tool: "run_query",
        connection: Some(("db", "Test DB")),
        raw_arguments: raw,
        statements,
        error,
        result_bytes: 99,
        redaction: RedactionSummary::default(),
        rules: rules(),
        dialect: Some(Dialect::Postgres),
    }
}

fn rules() -> &'static Redaction {
    static RULES: std::sync::OnceLock<Redaction> = std::sync::OnceLock::new();
    RULES.get_or_init(Redaction::default)
}

fn err(code: &str, index: Option<usize>) -> DraftError<'_> {
    DraftError {
        code,
        message: "m",
        denied_index: index,
    }
}

fn approvals(e: &AuditEntry) -> Vec<ApprovalOutcome> {
    e.statements.iter().map(|s| s.approval).collect()
}

#[test]
fn approval_table() {
    use ApprovalOutcome::{Approved, Denied, NotAsked, NotNeeded, Refused};
    let three = || {
        vec![
            st("a", true, false),
            st("b", false, false),
            st("c", true, false),
        ]
    };

    let e = AuditEntry::from_draft(draft("{}", vec![st("r", false, true)], None));
    assert_eq!((approvals(&e), e.approval), (vec![NotNeeded], NotNeeded));
    assert_eq!(e.outcome, CallOutcome::Ok);
    assert_eq!(
        (e.ts_ms, e.duration_ms, e.result_bytes, e.rows),
        (1234, 7, 99, 2)
    );

    let e = AuditEntry::from_draft(draft("{}", three(), None));
    assert_eq!(approvals(&e), vec![Approved, NotNeeded, Approved]);
    assert_eq!(e.approval, Approved);

    let e = AuditEntry::from_draft(draft("{}", three(), Some(err("denied", Some(2)))));
    assert_eq!(approvals(&e), vec![Approved, NotNeeded, Denied]);
    assert_eq!((e.approval, e.outcome), (Denied, CallOutcome::Error));

    let e = AuditEntry::from_draft(draft("{}", three(), Some(err("denied", Some(0)))));
    assert_eq!(approvals(&e), vec![Denied, NotNeeded, NotAsked]);

    for code in ["no_approver", "read_only"] {
        let e = AuditEntry::from_draft(draft("{}", three(), Some(err(code, None))));
        assert_eq!(approvals(&e), vec![Refused, NotNeeded, NotAsked], "{code}");
        assert_eq!(e.approval, Refused);
    }

    let e = AuditEntry::from_draft(draft("{}", three(), Some(err("driver", None))));
    assert_eq!(approvals(&e), vec![Approved, NotNeeded, Approved]);
}

#[test]
fn canaries_never_reach_the_serialized_line() {
    let sql = "ALTER USER u PASSWORD 'CANARY-PW-1'";
    let args = json!({
        "sql": sql,
        "connection": "db",
        "password": "CANARY-PW-2",
        "nested": {"api_key": "CANARY-PW-2", "note": "x"},
        "other": sql,
    })
    .to_string();
    let e = AuditEntry::from_draft(draft(
        &args,
        vec![st(sql, true, true)],
        Some(err("driver", None)),
    ));
    let line = String::from_utf8(serialize_line(&e)).unwrap();
    assert!(!line.contains("CANARY-PW"), "{line}");
    assert_eq!(e.arguments["connection"], "db");
    assert_eq!(e.arguments["nested"]["note"], "x");
    assert_eq!(e.arguments["password"], "<redacted>");
}

#[test]
fn the_error_message_is_redacted_and_capped() {
    let long = format!("PASSWORD 'CANARY-PW-3' {}", "é".repeat(2_000));
    let mut d = draft("{}", vec![], Some(err("driver", None)));
    d.error = Some(DraftError {
        code: "driver",
        message: &long,
        denied_index: None,
    });
    let e = AuditEntry::from_draft(d);
    let m = &e.error.unwrap().message;
    assert!(!m.contains("CANARY-PW-3"));
    assert!(m.len() <= MAX_AUDIT_TEXT_BYTES);
}

#[test]
fn caps_and_truncated_flags() {
    let big = "a".repeat(MAX_AUDIT_SQL_BYTES + 10);
    let args = json!({"sql": big, "text": "b".repeat(5_000), "n": 3}).to_string();
    let e = AuditEntry::from_draft(draft(&args, vec![st(&big, false, true)], None));
    assert!(e.arguments_truncated);
    assert_eq!(
        e.arguments["sql"].as_str().unwrap().len(),
        MAX_AUDIT_SQL_BYTES
    );
    assert_eq!(
        e.arguments["text"].as_str().unwrap().len(),
        MAX_AUDIT_TEXT_BYTES
    );
    assert_eq!(e.arguments["n"], 3);
    assert!(e.statements[0].sql_truncated);
    assert_eq!(e.statements[0].sql.len(), MAX_AUDIT_SQL_BYTES);

    let small = AuditEntry::from_draft(draft("{\"a\":\"b\"}", vec![st("x", false, true)], None));
    assert!(!small.arguments_truncated && !small.statements[0].sql_truncated);

    let many: Vec<_> = (0..70).map(|_| st("s", false, true)).collect();
    let e = AuditEntry::from_draft(draft("", many, None));
    assert_eq!(e.statements.len(), MAX_AUDIT_STATEMENTS);
    assert_eq!(e.statements_omitted, 6);
    assert_eq!(e.rows, 140);
    assert_eq!(e.arguments, json!({}));
}

#[test]
fn unparsable_arguments_are_kept_redacted_and_cut() {
    let raw = format!("not json PASSWORD 'CANARY-PW-4' {}", "x".repeat(3_000));
    let e = AuditEntry::from_draft(draft(&raw, vec![], None));
    let text = e.arguments["_unparsed"].as_str().unwrap();
    assert!(!text.contains("CANARY-PW-4"));
    assert!(text.len() <= MAX_AUDIT_TEXT_BYTES);
    assert!(e.arguments_truncated);
    let e = AuditEntry::from_draft(draft("[1,2]", vec![], None));
    assert_eq!(e.arguments, json!({"_unparsed": "[1,2]"}));
}

#[test]
fn statement_run_details_and_unique_ids() {
    let mut s = st("select 1", false, true);
    s.ran = Some(DraftRun {
        rows: 5,
        truncated: Some("row_limit"),
        failed: true,
    });
    let a = AuditEntry::from_draft(draft("{}", vec![s], None));
    let b = AuditEntry::from_draft(draft("{}", vec![], None));
    let s = &a.statements[0];
    assert_eq!(s.truncated.as_deref(), Some("row_limit"));
    assert!(s.ran && s.failed);
    assert_eq!(s.class, "read");
    assert_ne!(a.id, b.id);
    assert!(a.id.starts_with("4d2-"));
}

#[test]
fn memory_sink_keeps_entries_in_order() {
    let m = MemoryAudit::new();
    let a = AuditEntry::from_draft(draft("{}", vec![], None));
    m.record(&a);
    m.record(&a);
    assert_eq!(m.entries(), vec![a.clone(), a]);
}

#[test]
fn enums_round_trip_their_wire_names() {
    for s in ["assist", "session", "mcp", "agent", "unspecified"] {
        assert_eq!(Surface::from_wire(s).unwrap().as_str(), s);
    }
    for s in ["not_needed", "approved", "denied", "refused", "not_asked"] {
        assert_eq!(ApprovalOutcome::from_wire(s).unwrap().as_str(), s);
    }
    assert!(CallOutcome::from_wire("nope").is_none());
}

fn sql_line(rules: &Redaction, dialect: Option<Dialect>, sql: &str) -> String {
    let mut d = draft(r#"{"sql":"x"}"#, vec![st(sql, true, true)], None);
    d.rules = rules;
    d.dialect = dialect;
    let raw = json!({ "sql": sql }).to_string();
    d.raw_arguments = &raw;
    let e = AuditEntry::from_draft(d);
    String::from_utf8(serialize_line(&e)).unwrap()
}

#[test]
fn a_listed_word_masks_every_literal_of_the_statement() {
    let rules = Redaction::new(sqler_config::RedactionList::new(["iban".to_owned()]).unwrap());
    for sql in [
        "update acct set ssn = '123-45-6789' where id = 7",
        "insert into t (iban) values ('DE89370400440532013000')",
        "select * from t where ssn = 'CANARY-X' /* CANARY-Y */",
        "update acct set ssn = 123456789",
    ] {
        for dialect in [Some(Dialect::Postgres), Some(Dialect::MySql), None] {
            let line = sql_line(&rules, dialect, sql);
            for canary in ["123-45-6789", "DE89", "CANARY", "123456789", "= 7"] {
                assert!(!line.contains(canary), "{canary} in {line}");
            }
            assert!(line.contains('?'), "{line}");
        }
    }
}

#[test]
fn dollar_quoted_literals_are_masked_on_postgres() {
    let line = sql_line(
        &Redaction::default(),
        Some(Dialect::Postgres),
        "update acct set ssn = $q$CANARY-Z$q$",
    );
    assert!(!line.contains("CANARY-Z"), "{line}");
}

#[test]
fn a_user_added_entry_masks_literals_and_argument_keys() {
    let list = sqler_config::RedactionList::new(["loyalty_no".to_owned()]).unwrap();
    let rules = Redaction::new(list);
    let sql = "update c set loyalty_no = 'CANARY-L1'";
    let line = sql_line(&rules, Some(Dialect::Sqlite), sql);
    assert!(!line.contains("CANARY-L1"), "{line}");
    assert!(line.contains("update c set loyalty_no = ?"), "{line}");
    let mut d = draft(r#"{"loyalty_no":"CANARY-L2"}"#, vec![], None);
    d.rules = &rules;
    let line = String::from_utf8(serialize_line(&AuditEntry::from_draft(d))).unwrap();
    assert!(!line.contains("CANARY-L2"), "{line}");
}

#[test]
fn an_untainted_statement_is_kept_as_written() {
    let rules = Redaction::default();
    let line = sql_line(
        &rules,
        Some(Dialect::Postgres),
        "select * from t where id = 7",
    );
    assert!(line.contains("id = 7"), "{line}");
}

#[test]
fn the_tool_name_is_capped_and_scrubbed() {
    let long = "x".repeat(10_000);
    let mut d = draft("{}", vec![], None);
    d.tool = &long;
    let e = AuditEntry::from_draft(d);
    assert!(e.tool.len() <= MAX_AUDIT_TEXT_BYTES);
}
