// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tool set: the one entry every AI surface calls.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime};

use serde_json::{Value, json};
use sqler_ai_api::{ToolCall, ToolDefinition, ToolResult};
use sqler_catalog::Catalog;
use sqler_config::{AiConfig, RedactionList};
use sqler_driver_api::sqler_core::DbObject;
use sqler_sql::{Dialect, StatementClass};

use crate::approve::Approver;
use crate::assist::DataSearch;
use crate::audit::{
    AuditDraft, AuditEntry, AuditSink, CancelPoint, DraftError, DraftRun, DraftStatement, Surface,
};
use crate::error::ToolError;
use crate::external::{self, ExternalContext, ExternalTools};
use crate::limits::Limits;
use crate::redact::{RedactScope, Redaction, RedactionSummary};
use crate::schema::{self, names};
use crate::source::{ConnectionInfo, SessionSource};
use crate::ui::UiHost;
use crate::untrusted::wrap_untrusted;
use crate::{catalog_tools, query_tools};

/// The most entries of the function-definition cache; it is cleared when full.
const DEFINITION_CACHE_CAP: usize = 256;

/// Why a statement's rows stop short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Truncation {
    /// The statement returned more rows than the row cap.
    RowLimit,
    /// The deadline passed and the driver was cancelled.
    TimeLimit,
}

impl Truncation {
    /// The value in the result JSON: `row_limit` or `time_limit`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RowLimit => "row_limit",
            Self::TimeLimit => "time_limit",
        }
    }
}

/// A statement that was sent to the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExecutedStatement {
    /// The exact SQL sent.
    pub sql: String,
    /// Its class.
    pub class: StatementClass,
    /// Whether the approver explicitly approved it (a plain read needs none).
    pub approved: bool,
    /// Rows kept.
    pub rows: u64,
    /// Whether and why the rows were cut.
    pub truncated: Option<Truncation>,
    /// Whether the database reported an error for this statement.
    pub failed: bool,
}

/// A tool result plus what an audit log needs.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CallReport {
    /// The result to give back to the model.
    pub result: ToolResult,
    /// The id of the connection the call used, if it named one.
    pub connection: Option<String>,
    /// The statements sent to the driver, in order.
    pub executed: Vec<ExecutedStatement>,
    /// The audit entry of this call (built with or without a sink).
    pub audit: AuditEntry,
    /// What the redaction hid in the result.
    pub redaction: RedactionSummary,
}

/// How far a call got, as far as the guard cares.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Phase {
    /// Nothing is approved yet.
    #[default]
    Started,
    /// Every needed approval was granted; nothing was sent yet.
    Approved,
    /// Statements are being sent (or were); their results are not all recorded yet.
    Executing,
}

/// What a dispatched call leaves behind besides its content.
#[derive(Debug, Default)]
pub(crate) struct RanState {
    pub connection: Option<String>,
    pub executed: Vec<ExecutedStatement>,
    pub info: Option<ConnectionInfo>,
    /// Every planned statement: its SQL, class and whether it needs approval.
    pub planned: Vec<(String, StatementClass, bool)>,
    pub phase: Phase,
}

/// The state of one call, shared with the drop guard so a dropped future can still be audited.
///
/// The lock is never held across an await.
#[derive(Debug, Default, Clone)]
pub(crate) struct Ran {
    state: Arc<Mutex<RanState>>,
}

impl Ran {
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut RanState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn set_planned(&self, planned: Vec<(String, StatementClass, bool)>) {
        self.with(|s| s.planned = planned);
    }

    pub(crate) fn push_executed(&self, executed: ExecutedStatement) {
        self.with(|s| s.executed.push(executed));
    }

    pub(crate) fn set_phase(&self, phase: Phase) {
        self.with(|s| s.phase = phase);
    }

    fn set_connection(&self, info: &ConnectionInfo) {
        self.with(|s| {
            s.connection = Some(info.id.clone());
            s.info = Some(info.clone());
        });
    }

    /// Takes the state out (the call is over).
    fn take(&self) -> RanState {
        self.with(std::mem::take)
    }
}

/// Writes a best-effort `cancelled` audit entry if a call's future is dropped before `finish`.
///
/// Dropping is synchronous: the entry is built from the shared state and handed to the sink's
/// synchronous `record`. It never panics (a panic while building is caught) and never awaits.
struct CancelGuard<'a> {
    toolset: &'a ToolSet,
    call: &'a ToolCall,
    ran: Ran,
    started_at: SystemTime,
    started: Instant,
    armed: bool,
}

impl CancelGuard<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }

    fn record(&self) {
        let Some(sink) = &self.toolset.inner.audit else {
            return;
        };
        let state = self.ran.take();
        let inner = &self.toolset.inner;
        let dialect = state
            .info
            .as_ref()
            .and_then(|i| Dialect::from_driver_id(i.driver.as_str()));
        let point = if state.phase >= Phase::Approved {
            CancelPoint::AfterApproval
        } else {
            CancelPoint::BeforeApproval
        };
        let entry = AuditEntry::from_draft(AuditDraft {
            started: self.started_at,
            duration: self.started.elapsed(),
            surface: inner.surface,
            tool: &self.call.name,
            connection: state
                .info
                .as_ref()
                .map(|i| (i.id.as_str(), i.name.as_str())),
            raw_arguments: &self.call.arguments,
            statements: draft_statements(&state),
            error: None,
            result_bytes: 0,
            redaction: RedactionSummary::default(),
            rules: &inner.redaction,
            dialect,
            cancel: Some(point),
        });
        sink.record(&entry);
    }
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            // Never panic out of a drop (it may run while unwinding).
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.record()));
        }
    }
}

/// The planned statements with what is known of their run. Once statements are being sent
/// (`Phase::Executing`), one without a recorded result counts as run: it was sent or may have
/// been, and an audit errs on the side of saying so.
fn draft_statements(ran: &RanState) -> Vec<DraftStatement<'_>> {
    let in_flight = ran.phase == Phase::Executing;
    ran.planned
        .iter()
        .enumerate()
        .map(|(i, (sql, class, needs_approval))| DraftStatement {
            sql,
            class: *class,
            needs_approval: *needs_approval,
            ran: match ran.executed.get(i) {
                Some(e) => Some(DraftRun {
                    rows: e.rows,
                    truncated: e.truncated.map(Truncation::as_str),
                    failed: e.failed,
                }),
                None => in_flight.then_some(DraftRun {
                    rows: 0,
                    truncated: None,
                    failed: false,
                }),
            },
        })
        .collect()
}

type DefinitionKey = (String, DbObject);

pub(crate) struct Inner {
    pub source: Arc<dyn SessionSource>,
    pub limits: Limits,
    pub approver: Option<Arc<dyn Approver>>,
    pub ui: Option<Arc<dyn UiHost>>,
    pub redaction: Redaction,
    pub audit: Option<Arc<dyn AuditSink>>,
    pub surface: Surface,
    pub data_search: Option<Arc<dyn DataSearch>>,
    pub external: Option<Arc<dyn ExternalTools>>,
    definitions: Mutex<HashMap<DefinitionKey, Option<String>>>,
}

impl Inner {
    /// The connection named by `name` (an id, else a unique display name).
    pub(crate) fn resolve(&self, name: &str) -> Result<ConnectionInfo, ToolError> {
        let all = self.source.connections();
        if let Some(found) = all.iter().find(|c| c.id == name) {
            return Ok(found.clone());
        }
        let mut by_name = all.iter().filter(|c| c.name == name);
        match (by_name.next(), by_name.next()) {
            (Some(found), None) => Ok(found.clone()),
            _ => Err(ToolError::UnknownConnection(name.to_owned())),
        }
    }

    pub(crate) fn catalog(&self, info: &ConnectionInfo) -> Result<Catalog, ToolError> {
        self.source.catalog(&info.id).ok_or_else(|| {
            ToolError::NotAvailable(format!("no catalog for connection `{}`", info.id))
        })
    }

    pub(crate) fn ui(&self) -> Result<&dyn UiHost, ToolError> {
        self.ui.as_deref().ok_or_else(|| {
            ToolError::NotAvailable("this host has no editor or results tabs".to_owned())
        })
    }

    #[allow(clippy::option_option)] // outer: cache hit, inner: the definition may be absent
    pub(crate) fn cached_definition(&self, key: &DefinitionKey) -> Option<Option<String>> {
        self.definitions.lock().ok()?.get(key).cloned()
    }

    pub(crate) fn store_definition(&self, key: DefinitionKey, text: Option<String>) {
        if let Ok(mut cache) = self.definitions.lock() {
            if cache.len() >= DEFINITION_CACHE_CAP {
                cache.clear();
            }
            cache.insert(key, text);
        }
    }
}

/// The tools a model may call, with every guard behind them.
///
/// Built once per host from a [`SessionSource`] and [`Limits`], optionally with an
/// [`Approver`] (without one, every statement that is not a plain read is refused) and a
/// [`UiHost`] (without one, the UI-only tools are not listed and not callable). Cheap to clone.
///
/// # Guards
///
/// - Every statement is classified by `sqler-sql`; only `Read` runs unasked. Write, DDL, other
///   and unrecognized statements need approval per statement, showing the exact SQL.
/// - All approvals of a call are collected before the first statement runs; one denial stops
///   the call with nothing run.
/// - A `prod` connection asks twice per statement.
/// - `EXPLAIN ANALYZE` always needs approval.
/// - Row and time limits apply to every SQL call; on `prod` they cannot be switched off.
///   Exceeding them truncates, says so, and a timeout cancels the driver.
#[derive(Clone)]
pub struct ToolSet {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ToolSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSet")
            .field("limits", &self.inner.limits)
            .field("approver", &self.inner.approver.is_some())
            .field("ui_host", &self.inner.ui.is_some())
            .field("data_search", &self.inner.data_search.is_some())
            .field("external", &self.inner.external.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolSet {
    /// A tool set over `source` with `limits`, no approver and no UI host.
    #[must_use]
    pub fn new(source: Arc<dyn SessionSource>, limits: Limits) -> Self {
        Self {
            inner: Arc::new(Inner {
                source,
                limits,
                approver: None,
                ui: None,
                redaction: Redaction::default(),
                audit: None,
                surface: Surface::Unspecified,
                data_search: None,
                external: None,
                definitions: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// A tool set with the limits and the redaction list of `config`.
    #[must_use]
    pub fn configured(source: Arc<dyn SessionSource>, config: &AiConfig) -> Self {
        Self::new(source, Limits::from(&config.limits)).with_redaction(config.redaction.clone())
    }

    fn rebuilt(&self, change: impl FnOnce(&mut Inner)) -> Self {
        let mut inner = Inner {
            source: Arc::clone(&self.inner.source),
            limits: self.inner.limits,
            approver: self.inner.approver.clone(),
            ui: self.inner.ui.clone(),
            redaction: self.inner.redaction.clone(),
            audit: self.inner.audit.clone(),
            surface: self.inner.surface,
            data_search: self.inner.data_search.clone(),
            external: self.inner.external.clone(),
            definitions: Mutex::new(HashMap::new()),
        };
        change(&mut inner);
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Sets the approver.
    #[must_use]
    pub fn with_approver(&self, approver: Arc<dyn Approver>) -> Self {
        self.rebuilt(|i| i.approver = Some(approver))
    }

    /// Sets the UI host.
    #[must_use]
    pub fn with_ui_host(&self, ui: Arc<dyn UiHost>) -> Self {
        self.rebuilt(|i| i.ui = Some(ui))
    }

    /// Sets the redaction list (the default is the built-in list).
    #[must_use]
    pub fn with_redaction(&self, list: RedactionList) -> Self {
        self.rebuilt(|i| i.redaction = Redaction::new(list))
    }

    /// Sets the audit sink and the surface written to every entry.
    #[must_use]
    pub fn with_audit(&self, sink: Arc<dyn AuditSink>, surface: Surface) -> Self {
        self.rebuilt(|i| {
            i.audit = Some(sink);
            i.surface = surface;
        })
    }

    /// Attaches the host that writes SQL for `search_data`. Only then is the tool listed and
    /// callable. The host's own tool set must not hold this one (see [`DataSearch`]).
    #[must_use]
    pub fn with_data_search(&self, search: Arc<dyn DataSearch>) -> Self {
        self.rebuilt(|i| i.data_search = Some(search))
    }

    /// Attaches a host that supplies extra tools (for example from plugins). They are listed
    /// after the built-in tools and run through the same guards, redaction and audit.
    #[must_use]
    pub fn with_external_tools(&self, tools: Arc<dyn ExternalTools>) -> Self {
        self.rebuilt(|i| i.external = Some(tools))
    }

    /// The redaction rules every result of this tool set passes.
    #[must_use]
    pub fn redaction(&self) -> &Redaction {
        &self.inner.redaction
    }

    /// The connection named by `name` (an id, else a unique display name).
    ///
    /// # Errors
    /// [`ToolError::UnknownConnection`] if the host lends no such connection.
    pub fn connection_info(&self, name: &str) -> Result<ConnectionInfo, ToolError> {
        self.inner.resolve(name)
    }

    /// The tool definitions to export to providers and MCP. `search_data` is included only
    /// when a [`DataSearch`] is attached.
    #[must_use]
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = schema::definitions(self.inner.ui.is_some());
        if self.inner.data_search.is_some() {
            tools.push(schema::search_data_definition());
        }
        if let Some(external) = &self.inner.external {
            tools.extend(
                external::listed(external.as_ref())
                    .into_iter()
                    .map(|t| ToolDefinition::new(t.name, t.description, t.input_schema)),
            );
        }
        tools
    }

    /// Runs one tool call. Never fails: errors become error results.
    pub async fn call(&self, call: &ToolCall) -> ToolResult {
        self.call_detailed(call).await.result
    }

    /// Runs one tool call and reports what ran.
    pub async fn call_detailed(&self, call: &ToolCall) -> CallReport {
        let started_at = SystemTime::now();
        let started = Instant::now();
        let mut ran = Ran::default();
        let mut guard = CancelGuard {
            toolset: self,
            call,
            ran: ran.clone(),
            started_at,
            started,
            armed: true,
        };
        let outcome = self.run(call, &mut ran).await;
        guard.disarm();
        self.finish(call, started_at, started, outcome, ran.take())
    }

    /// The one exit of every call: redact, wrap, build the result, audit.
    fn finish(
        &self,
        call: &ToolCall,
        started_at: SystemTime,
        started: Instant,
        outcome: Result<Value, ToolError>,
        ran: RanState,
    ) -> CallReport {
        let denied_index = match &outcome {
            Err(ToolError::Denied { index }) => Some(*index),
            _ => None,
        };
        let code = outcome.as_ref().err().map(ToolError::code);
        let failed = outcome.is_err();
        let mut body = match outcome {
            Ok(content) => content,
            Err(err) => error_json(&err),
        };
        let (send_rows, dialect) = ran.info.as_ref().map_or((false, None), |i| {
            (i.send_rows, Dialect::from_driver_id(i.driver.as_str()))
        });
        let summary = self
            .inner
            .redaction
            .apply(&mut body, RedactScope::new(send_rows, dialect));
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let content = wrap_untrusted(&body);
        let result_bytes = content.len();
        let result = if failed {
            ToolResult::error(&call.id, content)
        } else {
            ToolResult::ok(&call.id, content)
        };
        let statements = draft_statements(&ran);
        let audit = AuditEntry::from_draft(AuditDraft {
            started: started_at,
            duration: started.elapsed(),
            surface: self.inner.surface,
            tool: &call.name,
            connection: ran.info.as_ref().map(|i| (i.id.as_str(), i.name.as_str())),
            raw_arguments: &call.arguments,
            statements,
            error: code.map(|code| DraftError {
                code,
                message: &message,
                denied_index,
            }),
            result_bytes,
            redaction: summary.clone(),
            rules: &self.inner.redaction,
            dialect,
            cancel: None,
        });
        if let Some(sink) = &self.inner.audit {
            sink.record(&audit);
        }
        CallReport {
            result,
            connection: ran.connection,
            executed: ran.executed,
            audit,
            redaction: summary,
        }
    }

    async fn run(&self, call: &ToolCall, ran: &mut Ran) -> Result<Value, ToolError> {
        let args = if call.arguments.trim().is_empty() {
            json!({})
        } else {
            call.parse_arguments()
                .map_err(|e| ToolError::InvalidArguments(e.to_string()))?
        };
        self.dispatch(&call.name, args, ran).await
    }

    /// The one place a tool name becomes a tool.
    async fn dispatch(&self, name: &str, args: Value, ran: &mut Ran) -> Result<Value, ToolError> {
        let ts = &*self.inner;
        match name {
            names::LIST_CONNECTIONS => {
                schema::parse::<schema::NoArgs>(args)?;
                Ok(catalog_tools::list_connections(ts))
            }
            names::LIST_SCHEMAS => {
                let a: schema::ListSchemasArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::list_schemas(ts, &info, &a).await
            }
            names::LIST_TABLES => {
                let a: schema::ListTablesArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::list_tables(ts, &info, &a).await
            }
            names::DESCRIBE_TABLE => {
                let a: schema::DescribeArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::describe_table(ts, &info, &a).await
            }
            names::DESCRIBE_FUNCTION => {
                let a: schema::DescribeArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::describe_function(ts, &info, &a).await
            }
            names::SEARCH_SCHEMA => {
                let a: schema::SearchSchemaArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                catalog_tools::search_schema(ts, &info, &a)
            }
            names::SAMPLE_ROWS => {
                let a: schema::SampleRowsArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                query_tools::sample_rows(ts, &info, &a, ran).await
            }
            names::RUN_QUERY => {
                let a: schema::RunQueryArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                query_tools::run_query(ts, &info, &a, ran).await
            }
            names::EXPLAIN_QUERY => {
                let a: schema::ExplainArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                query_tools::explain_query(ts, &info, &a, ran).await
            }
            names::OPEN_IN_EDITOR => {
                let a: schema::OpenInEditorArgs = schema::parse(args)?;
                let info = match &a.connection {
                    Some(name) => Some(self.connection(name, ran)?),
                    None => None,
                };
                query_tools::open_in_editor(ts, info.as_ref(), &a).await
            }
            names::OPEN_RESULTS_TAB => {
                let a: schema::OpenResultsTabArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                query_tools::open_results_tab(ts, &info, &a).await
            }
            names::SEARCH_DATA => {
                let Some(search) = ts.data_search.as_deref() else {
                    return Err(ToolError::NotAvailable(
                        "search_data is not available in this host".to_owned(),
                    ));
                };
                let a: schema::SearchDataArgs = schema::parse(args)?;
                let info = self.connection(&a.connection, ran)?;
                crate::assist::search_data(ts, search, &info, &a, ran).await
            }
            other => self.call_external(other, args, ran).await,
        }
    }

    /// A name no built-in arm took: an external tool, or unknown.
    async fn call_external(&self, name: &str, args: Value, ran: &Ran) -> Result<Value, ToolError> {
        let unknown = || ToolError::UnknownTool(name.to_owned());
        let external = self.inner.external.as_ref().ok_or_else(unknown)?;
        let tool = external::listed(external.as_ref())
            .into_iter()
            .find(|t| t.name == name)
            .ok_or_else(unknown)?;
        external::check_arguments(&tool.input_schema, &args)?;
        let ctx = ExternalContext::new(Arc::clone(&self.inner), ran.clone());
        let _close = ctx.close_on_drop();
        let outcome = external.call(name, args, ctx.clone()).await;
        ctx.verdict()?;
        outcome
    }

    fn connection(&self, name: &str, ran: &Ran) -> Result<ConnectionInfo, ToolError> {
        let info = self.inner.resolve(name)?;
        ran.set_connection(&info);
        Ok(info)
    }
}

/// The error result body: `{"error":{"code":..,"message":..}}`.
fn error_json(err: &ToolError) -> Value {
    json!({"error": {"code": err.code(), "message": err.to_string()}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_body_has_code_and_message() {
        let body = error_json(&ToolError::UnknownTool("zap".into()));
        assert_eq!(body["error"]["code"], "unknown_tool");
        assert_eq!(body["error"]["message"], "unknown tool: zap");
    }

    #[test]
    fn truncation_names() {
        assert_eq!(Truncation::RowLimit.as_str(), "row_limit");
        assert_eq!(Truncation::TimeLimit.as_str(), "time_limit");
    }
}
