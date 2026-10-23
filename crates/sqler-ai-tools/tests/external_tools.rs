// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! External tools: listed after the built-ins, checked, guarded, redacted and audited like any
//! other tool call (ADR-0062).
#![allow(
    clippy::unwrap_used,
    clippy::needless_pass_by_value,
    clippy::assert_is_empty,
    clippy::items_after_statements
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{ScriptedApprover, SqliteSource, call, limits};
use serde_json::{Value, json};
use sqler_ai_api::{BoxFuture, ToolCall};
use sqler_ai_tools::{
    ApprovalOutcome, ApprovalReason, ApprovalStage, AuditEntry, CallOutcome, Decision,
    EXTERNAL_TOOL, ExternalApproval, ExternalContext, ExternalTool, ExternalTools,
    MAX_EXTERNAL_DESCRIPTION_BYTES, MemoryAudit, REDACTED, Surface, ToolError, ToolSet, names,
    unwrap_untrusted,
};
use sqler_driver_api::sqler_core::{Environment, Value as SqlValue};
use sqler_sql::StatementClass;

const INSERT: &str = "insert into users (id, name) values (9, 'zed')";

fn object_schema(extra: Value) -> Value {
    let mut schema = json!({"type": "object", "properties": {}});
    if let (Some(base), Some(more)) = (schema.as_object_mut(), extra.as_object()) {
        for (k, v) in more {
            base.insert(k.clone(), v.clone());
        }
    }
    schema
}

/// The fake host: a handful of tools that exercise the context.
struct Fake {
    extra: Vec<ExternalTool>,
    /// A context a tool kept past its call.
    leaked: std::sync::Mutex<Option<ExternalContext>>,
}

impl Fake {
    fn new() -> Arc<Self> {
        Self::with(Vec::new())
    }

    fn with(extra: Vec<ExternalTool>) -> Arc<Self> {
        Arc::new(Self {
            extra,
            leaked: std::sync::Mutex::default(),
        })
    }
}

fn tool(name: &str, schema: Value) -> ExternalTool {
    ExternalTool::new(name, format!("the {name} tool"), schema)
}

fn approval(sql: &str, stage: ApprovalStage) -> ExternalApproval {
    ExternalApproval::new(
        "db",
        sql,
        Vec::new(),
        StatementClass::Write,
        ApprovalReason::Write,
        stage,
    )
}

impl ExternalTools for Fake {
    fn tools(&self) -> Vec<ExternalTool> {
        let mut tools = vec![
            tool("fake__echo", object_schema(json!({}))),
            tool("fake__write", object_schema(json!({}))),
            tool("fake__write_twice", object_schema(json!({}))),
            tool("fake__ignores_denial", object_schema(json!({}))),
            tool("fake__params", object_schema(json!({}))),
            tool("fake__foreign", object_schema(json!({}))),
            tool("fake__rows", object_schema(json!({}))),
            tool("fake__hang", object_schema(json!({}))),
            tool("fake__fail", object_schema(json!({}))),
            tool("fake__leak", object_schema(json!({}))),
            tool(
                "fake__strict",
                json!({
                    "type": "object",
                    "required": ["name"],
                    "additionalProperties": false,
                    "properties": {
                        "name": {"type": "string", "enum": ["a", "b"]},
                        "n": {"type": ["integer", "null"]},
                    },
                }),
            ),
        ];
        tools.extend(self.extra.clone());
        tools
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
        ctx: ExternalContext,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            match name {
                "fake__echo" => Ok(arguments),
                "fake__write" => {
                    let d = ctx.approve(approval(INSERT, ApprovalStage::Confirm)).await;
                    Ok(json!({"approved": d == Decision::Approve}))
                }
                "fake__write_twice" => {
                    let a = ctx.approve(approval(INSERT, ApprovalStage::Confirm)).await;
                    let b = ctx
                        .approve(approval("delete from users", ApprovalStage::Confirm))
                        .await;
                    Ok(json!({"first": a == Decision::Approve, "second": b == Decision::Approve}))
                }
                "fake__ignores_denial" => {
                    let _ = ctx.approve(approval(INSERT, ApprovalStage::Confirm)).await;
                    Ok(json!({"done": true}))
                }
                "fake__params" => {
                    let mut request = approval(INSERT, ApprovalStage::Confirm);
                    request.params = vec![SqlValue::Int(1)];
                    let d = ctx.approve(request).await;
                    Ok(json!({"approved": d == Decision::Approve}))
                }
                "fake__foreign" => {
                    ctx.use_connection("nope")?;
                    Ok(json!({}))
                }
                "fake__rows" => {
                    ctx.use_connection("db")?;
                    Ok(json!({
                        "columns": [{"name": "id"}, {"name": "password"}],
                        "rows": [[1, "CANARY-PW"]],
                    }))
                }
                "fake__hang" => {
                    *self.leaked.lock().unwrap() = Some(ctx.clone());
                    let _ = ctx.approve(approval(INSERT, ApprovalStage::Confirm)).await;
                    std::future::pending::<()>().await;
                    Ok(json!({}))
                }
                "fake__fail" => Err(ToolError::NotFound("nothing here".into())),
                "fake__leak" => {
                    *self.leaked.lock().unwrap() = Some(ctx);
                    Ok(json!({}))
                }
                _ => Err(ToolError::UnknownTool(name.to_owned())),
            }
        })
    }
}

struct Rig {
    ts: ToolSet,
    audit: Arc<MemoryAudit>,
}

async fn rig(
    env: Option<Environment>,
    approver: Option<Arc<ScriptedApprover>>,
    external: Arc<Fake>,
) -> Rig {
    let source = SqliteSource::seeded("external", env, Some(true), common::SEED).await;
    let audit = Arc::new(MemoryAudit::new());
    let mut ts = ToolSet::new(source, limits());
    if let Some(approver) = approver {
        ts = ts.with_approver(approver);
    }
    let ts = ts
        .with_external_tools(external)
        .with_audit(audit.clone(), Surface::Mcp);
    Rig { ts, audit }
}

fn only(audit: &MemoryAudit) -> AuditEntry {
    let mut entries = audit.entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    entries.pop().unwrap()
}

fn error_code(content: &Value) -> &str {
    content["error"]["code"].as_str().unwrap()
}

#[tokio::test]
async fn listed_after_the_built_ins_and_filtered() {
    let long = ExternalTool::new("fake__long", "é".repeat(2000), object_schema(json!({})));
    let rig = rig(
        None,
        None,
        Fake::with(vec![
            tool("run_query", object_schema(json!({}))),
            tool("search_data", object_schema(json!({}))),
            tool("nounderscores", object_schema(json!({}))),
            tool("bad name__x", object_schema(json!({}))),
            tool(
                &format!("fake__{}", "x".repeat(70)),
                object_schema(json!({})),
            ),
            tool(
                "fake__echo",
                json!({"type": "object", "description": "dup"}),
            ),
            tool("fake__array", json!({"type": "array"})),
            tool("fake__notype", json!({"properties": {}})),
            long,
        ]),
    )
    .await;
    let plain = ToolSet::new(SqliteSource::new("plain", None).await, limits()).definitions();
    let defs = rig.ts.definitions();
    let names_now: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        &names_now[..plain.len()],
        plain.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()
    );
    let external: Vec<&str> = names_now[plain.len()..].to_vec();
    assert_eq!(
        external,
        [
            "fake__echo",
            "fake__write",
            "fake__write_twice",
            "fake__ignores_denial",
            "fake__params",
            "fake__foreign",
            "fake__rows",
            "fake__hang",
            "fake__fail",
            "fake__leak",
            "fake__strict",
            "fake__long",
        ]
    );
    // The first of two duplicates wins.
    let echo = defs.iter().find(|d| d.name == "fake__echo").unwrap();
    assert_eq!(echo.description, "the fake__echo tool");
    let long = defs.iter().find(|d| d.name == "fake__long").unwrap();
    assert!(long.description.len() <= MAX_EXTERNAL_DESCRIPTION_BYTES);
    assert!(long.description.chars().all(|c| c == 'é'));
}

#[tokio::test]
async fn without_a_host_the_name_is_unknown() {
    let source = SqliteSource::new("none", None).await;
    let ts = ToolSet::new(source, limits());
    let (_, content) = call(&ts, "fake__echo", json!({})).await;
    assert_eq!(error_code(&content), "unknown_tool");
    let rig = rig(None, None, Fake::new()).await;
    let (_, content) = call(&rig.ts, "fake__missing", json!({})).await;
    assert_eq!(error_code(&content), "unknown_tool");
}

#[tokio::test]
async fn result_is_enveloped_and_audited() {
    let rig = rig(None, None, Fake::new()).await;
    let report = rig
        .ts
        .call_detailed(&ToolCall::new(
            "1",
            "fake__echo",
            json!({"k": "v"}).to_string(),
        ))
        .await;
    assert!(!report.result.is_error);
    let body: Value =
        serde_json::from_str(unwrap_untrusted(&report.result.content).unwrap()).unwrap();
    assert_eq!(body, json!({"k": "v"}));
    let entry = only(&rig.audit);
    assert_eq!(entry.tool, "fake__echo");
    assert_eq!(entry.surface, Surface::Mcp);
    assert_eq!(entry.outcome, CallOutcome::Ok);
    assert!(entry.statements.is_empty());
}

#[tokio::test]
async fn schema_violations_do_not_reach_the_tool() {
    let rig = rig(None, None, Fake::new()).await;
    for args in [
        json!({}),
        json!({"name": "c"}),
        json!({"name": 1}),
        json!({"name": "a", "n": 1.5}),
        json!({"name": "a", "extra": 1}),
    ] {
        let (report, content) = call(&rig.ts, "fake__strict", args).await;
        assert!(report.result.is_error);
        assert_eq!(error_code(&content), "invalid_arguments");
    }
    let (_, ok) = call(&rig.ts, "fake__strict", json!({"name": "a", "n": null})).await;
    // `fake__strict` is not implemented by the fake: reaching it proves the check passed.
    assert_eq!(error_code(&ok), "unknown_tool");
    let report = rig
        .ts
        .call_detailed(&ToolCall::new("1", "fake__echo", "[1]"))
        .await;
    assert!(report.result.is_error);
    assert_eq!(rig.audit.entries().len(), 7);
}

#[tokio::test]
async fn row_values_of_listed_columns_are_masked() {
    let rig = rig(None, None, Fake::new()).await;
    let (report, content) = call(&rig.ts, "fake__rows", json!({})).await;
    assert!(!report.result.is_error);
    assert_eq!(content["rows"][0][0], 1);
    assert_eq!(content["rows"][0][1], REDACTED);
    assert!(!report.result.content.contains("CANARY-PW"));
    assert_eq!(only(&rig.audit).connection.unwrap().id, "db");
}

#[tokio::test]
async fn an_approved_statement_is_recorded() {
    let approver = ScriptedApprover::always(Decision::Approve);
    let rig = rig(None, Some(approver.clone()), Fake::new()).await;
    let (report, content) = call(&rig.ts, "fake__write", json!({})).await;
    assert!(!report.result.is_error);
    assert_eq!(content["approved"], true);
    let asked = approver.requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].tool, EXTERNAL_TOOL);
    assert_eq!(asked[0].sql, INSERT);
    assert_eq!(asked[0].connection.id, "db");
    assert_eq!((asked[0].index, asked[0].count), (0, 1));
    assert_eq!(report.executed.len(), 1);
    assert!(report.executed[0].approved);
    let entry = only(&rig.audit);
    assert_eq!(entry.approval, ApprovalOutcome::Approved);
    assert_eq!(entry.statements.len(), 1);
    assert_eq!(entry.statements[0].sql, INSERT);
}

#[tokio::test]
async fn prod_asks_twice() {
    let approver = ScriptedApprover::always(Decision::Approve);
    let source =
        SqliteSource::seeded("prod", Some(Environment::Prod), Some(true), common::SEED).await;
    // A tool that asks Confirm, then ProdSecond.
    struct Staged;
    impl ExternalTools for Staged {
        fn tools(&self) -> Vec<ExternalTool> {
            vec![tool("fake__staged", object_schema(json!({})))]
        }
        fn call<'a>(
            &'a self,
            _: &'a str,
            _: Value,
            ctx: ExternalContext,
        ) -> BoxFuture<'a, Result<Value, ToolError>> {
            Box::pin(async move {
                let a = ctx.approve(approval(INSERT, ApprovalStage::Confirm)).await;
                let b = ctx
                    .approve(approval(INSERT, ApprovalStage::ProdSecond))
                    .await;
                Ok(json!({"a": a == Decision::Approve, "b": b == Decision::Approve}))
            })
        }
    }
    let ts = ToolSet::new(source, limits())
        .with_approver(approver.clone())
        .with_external_tools(Arc::new(Staged));
    let report = ts
        .call_detailed(&ToolCall::new("1", "fake__staged", "{}"))
        .await;
    assert!(!report.result.is_error);
    let asked = approver.requests();
    let stages: Vec<_> = asked.iter().map(|r| (r.stage, r.index)).collect();
    assert_eq!(
        stages,
        [(ApprovalStage::Confirm, 0), (ApprovalStage::ProdSecond, 0)]
    );
    assert_eq!(report.executed.len(), 1);
}

#[tokio::test]
async fn a_denial_fails_the_call_and_later_asks_are_not_made() {
    let approver = ScriptedApprover::always(Decision::Deny);
    let rig = rig(None, Some(approver.clone()), Fake::new()).await;
    let (report, content) = call(&rig.ts, "fake__write_twice", json!({})).await;
    assert!(report.result.is_error);
    assert_eq!(error_code(&content), "denied");
    assert_eq!(approver.requests().len(), 1);
    assert!(report.executed.is_empty());
    let entry = only(&rig.audit);
    assert_eq!(entry.approval, ApprovalOutcome::Denied);
    assert_eq!(entry.statements.len(), 1);
}

#[tokio::test]
async fn a_tool_cannot_hide_a_denial() {
    let approver = ScriptedApprover::always(Decision::Deny);
    let rig = rig(None, Some(approver), Fake::new()).await;
    let (report, content) = call(&rig.ts, "fake__ignores_denial", json!({})).await;
    assert!(report.result.is_error);
    assert_eq!(error_code(&content), "denied");
}

#[tokio::test]
async fn without_an_approver_nothing_is_asked() {
    let rig = rig(None, None, Fake::new()).await;
    let (report, content) = call(&rig.ts, "fake__write", json!({})).await;
    assert!(report.result.is_error);
    assert_eq!(error_code(&content), "no_approver");
    assert!(report.executed.is_empty());
    assert_eq!(only(&rig.audit).error.unwrap().code, "no_approver");
}

#[tokio::test]
async fn parameters_are_denied_unasked() {
    let approver = ScriptedApprover::always(Decision::Approve);
    let rig = rig(None, Some(approver.clone()), Fake::new()).await;
    let (report, content) = call(&rig.ts, "fake__params", json!({})).await;
    assert!(report.result.is_error);
    assert_eq!(error_code(&content), "denied");
    assert!(approver.requests().is_empty());
    assert!(report.executed.is_empty());
}

#[tokio::test]
async fn a_foreign_connection_id_fails() {
    let rig = rig(None, None, Fake::new()).await;
    let (report, content) = call(&rig.ts, "fake__foreign", json!({})).await;
    assert!(report.result.is_error);
    assert_eq!(error_code(&content), "unknown_connection");
}

#[tokio::test]
async fn a_tool_error_stays_an_error_result() {
    let rig = rig(None, None, Fake::new()).await;
    let (report, content) = call(&rig.ts, "fake__fail", json!({})).await;
    assert!(report.result.is_error);
    assert_eq!(error_code(&content), "not_found");
    assert_eq!(only(&rig.audit).outcome, CallOutcome::Error);
}

#[tokio::test(flavor = "current_thread")]
async fn a_dropped_call_leaves_a_cancelled_row() {
    let approver = ScriptedApprover::always(Decision::Approve);
    let rig = rig(None, Some(approver), Fake::new()).await;
    let call = ToolCall::new("1", "fake__hang", "{}");
    let timed = tokio::time::timeout(Duration::from_millis(200), rig.ts.call_detailed(&call)).await;
    assert!(timed.is_err());
    let entry = only(&rig.audit);
    assert_eq!(entry.outcome, CallOutcome::Cancelled);
    assert_eq!(entry.tool, "fake__hang");
    assert_eq!(entry.statements.len(), 1);
    assert_eq!(entry.statements[0].approval, ApprovalOutcome::Approved);
    // Approved statements count as sent: the tool may have run it.
    assert!(entry.statements[0].ran);
}

#[tokio::test]
async fn a_leaked_context_is_closed_after_the_call() {
    let approver = ScriptedApprover::always(Decision::Approve);
    let fake = Fake::new();
    let rig = rig(None, Some(approver.clone()), fake.clone()).await;
    let (report, _) = call(&rig.ts, "fake__leak", json!({})).await;
    assert!(!report.result.is_error);
    let ctx = fake.leaked.lock().unwrap().take().unwrap();
    let d = ctx.approve(approval(INSERT, ApprovalStage::Confirm)).await;
    assert_eq!(d, Decision::Deny);
    assert!(approver.requests().is_empty());
    assert!(matches!(
        ctx.use_connection("db"),
        Err(ToolError::NotAvailable(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_call_closes_its_context() {
    let fake = Fake::new();
    let rig = rig(None, None, fake.clone()).await;
    let call = ToolCall::new("1", "fake__hang", "{}");
    let timed = tokio::time::timeout(Duration::from_millis(100), rig.ts.call_detailed(&call)).await;
    assert!(timed.is_err());
    let ctx = fake.leaked.lock().unwrap().take().unwrap();
    assert!(matches!(
        ctx.use_connection("db"),
        Err(ToolError::NotAvailable(_))
    ));
}

#[tokio::test]
async fn a_read_only_connection_is_refused_before_asking() {
    use sqler_ai_tools::{ConnectionInfo, SessionSource};
    struct ReadOnly(Arc<SqliteSource>);
    impl SessionSource for ReadOnly {
        fn connections(&self) -> Vec<ConnectionInfo> {
            vec![self.0.info.clone().with_read_only(true)]
        }
        fn catalog(&self, c: &str) -> Option<sqler_catalog::Catalog> {
            self.0.catalog(c)
        }
        fn acquire<'a>(
            &'a self,
            c: &'a str,
        ) -> BoxFuture<'a, Result<Box<dyn sqler_driver_api::Session>, ToolError>> {
            self.0.acquire(c)
        }
    }
    let inner = SqliteSource::seeded("ro", None, Some(true), common::SEED).await;
    let approver = ScriptedApprover::always(Decision::Approve);
    let ts = ToolSet::new(Arc::new(ReadOnly(inner)), limits())
        .with_approver(approver.clone())
        .with_external_tools(Fake::new());
    let (report, content) = call(&ts, "fake__write", json!({})).await;
    assert!(report.result.is_error);
    assert_eq!(error_code(&content), "denied");
    assert!(approver.requests().is_empty());
    assert!(report.executed.is_empty());
}

#[tokio::test]
async fn built_ins_are_untouched_by_the_host() {
    let rig = rig(None, None, Fake::new()).await;
    let (report, content) = call(&rig.ts, names::LIST_CONNECTIONS, json!({})).await;
    assert!(!report.result.is_error);
    assert!(content.to_string().contains("db"));
}
