// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin tools for the AI surfaces (ADR-0062): [`AppAiTools`] implements the tool contract's `ExternalTools` over the
//! plugin host.
//!
//! The tool set of a surface lists the tools, checks the arguments, and redacts, wraps and audits the result. This
//! adapter only runs the plugin. For each call it lends the plugin a [`PluginData`] of that call alone:
//!
//! - [`SurfaceData`] shows only the connections the surface lends (a plugin never reaches a connection the AI
//!   opt-in left out), with the surface's read-only flag applied at the data layer;
//! - [`SurfaceApprover`] sends every write approval to the surface's own approver (through the call's context), never
//!   to the plugin manager's approval dialog. A surface with no approver refuses every write.
//!
//! Nothing here builds the plugin engine: [`ExternalTools::tools`] lists what an earlier refresh found.

use std::sync::Arc;

use serde_json::Value;
use sqler_ai_api::BoxFuture;
use sqler_ai_tools::{
    ApprovalReason, ApprovalStage as AiStage, Decision as AiDecision, ExternalApproval,
    ExternalContext, ExternalTool, ExternalTools, ToolError,
};
use sqler_catalog::Catalog;
use sqler_driver_api::Session;
use sqler_plugin_host::{
    ApprovalStage, CallError, DataError, DataService, Decision, ErrorCode, PluginConnection,
    PluginData, QueryApprover, WriteReason, WriteRequest,
};

use super::backend::AppPlugins;
use super::data::AppData;

/// The longest guest message passed to the model, in characters.
const MAX_MESSAGE_CHARS: usize = 512;

/// The plugin tools on offer to the AI surfaces.
pub(super) struct AppAiTools {
    plugins: Arc<AppPlugins>,
    data: Arc<AppData>,
}

impl AppAiTools {
    pub(super) fn new(plugins: Arc<AppPlugins>, data: Arc<AppData>) -> Self {
        Self { plugins, data }
    }
}

fn clip(text: &str) -> String {
    text.chars().take(MAX_MESSAGE_CHARS).collect()
}

/// What the model is told about a failed call. Engine text never reaches it; guest messages are clipped.
fn map_error(error: CallError) -> ToolError {
    match error {
        CallError::Guest(g) => {
            let message = clip(&g.message);
            match g.code {
                ErrorCode::InvalidArgument => ToolError::InvalidArguments(message),
                ErrorCode::NotFound => ToolError::NotFound(message),
                ErrorCode::CapabilityDenied => ToolError::NotAvailable(message),
                _ => ToolError::Host(format!("the plugin tool failed: {message}")),
            }
        }
        CallError::Timeout { .. } => {
            ToolError::Host("the plugin tool did not finish in time".to_owned())
        }
        CallError::Cancelled => ToolError::Host("the plugin tool was cancelled".to_owned()),
        CallError::Disabled
        | CallError::NotFound
        | CallError::Load(_)
        | CallError::MissingExport { .. } => {
            ToolError::NotAvailable("the plugin tool is not available".to_owned())
        }
        // Traps, limits, panics, engine errors and whatever the host adds later: no engine text.
        _ => ToolError::Host("the plugin tool failed".to_owned()),
    }
}

impl ExternalTools for AppAiTools {
    fn tools(&self) -> Vec<ExternalTool> {
        let Some(host) = self.plugins.built() else {
            return Vec::new();
        };
        host.ai_tools()
            .into_iter()
            .map(|t| {
                ExternalTool::new(
                    t.model_name(),
                    format!(
                        "Plugin tool from {} ({}): {}",
                        t.plugin_name, t.plugin, t.tool.description
                    ),
                    t.tool.input_schema.clone(),
                )
            })
            .collect()
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
        ctx: ExternalContext,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            let host = self
                .plugins
                .built()
                .ok_or_else(|| ToolError::UnknownTool(name.to_owned()))?;
            let tool = host
                .ai_tools()
                .into_iter()
                .find(|t| t.model_name() == name)
                .ok_or_else(|| ToolError::UnknownTool(name.to_owned()))?;
            let data = PluginData::new(Arc::new(SurfaceData {
                data: Arc::clone(&self.data),
                ctx: ctx.clone(),
            }))
            .with_approver(Arc::new(SurfaceApprover { ctx }));
            let text = host
                .invoke_ai_tool(&tool.plugin, &tool.tool.name, arguments.to_string(), data)
                .await
                .map_err(map_error)?;
            serde_json::from_str(&text)
                .map_err(|_| ToolError::Host("the plugin tool returned invalid JSON".to_owned()))
        })
    }
}

/// The databases a plugin may use during one AI call: the connections of the surface, read-only where it says so.
struct SurfaceData {
    data: Arc<AppData>,
    ctx: ExternalContext,
}

impl DataService for SurfaceData {
    fn connections(&self) -> Vec<PluginConnection> {
        self.ctx
            .connections()
            .into_iter()
            .map(|c| {
                PluginConnection::new(c.id, c.name, c.driver, c.environment)
                    .with_read_only(c.read_only)
            })
            .collect()
    }

    fn catalog(&self, connection: &str) -> Option<Catalog> {
        // Only a connection of the surface has a catalog.
        self.ctx.use_connection(connection).ok()?;
        self.data.catalog(connection)
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DataError>> {
        Box::pin(async move {
            self.ctx
                .use_connection(connection)
                .map_err(|_| DataError::UnknownConnection(connection.to_owned()))?;
            self.data.acquire(connection).await
        })
    }

    fn release(&self, connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        self.data.release(connection, session)
    }
}

/// Sends the approvals of a plugin statement to the approver of the surface that made the call.
struct SurfaceApprover {
    ctx: ExternalContext,
}

fn reason_of(reason: WriteReason) -> ApprovalReason {
    match reason {
        WriteReason::Write => ApprovalReason::Write,
        WriteReason::Ddl => ApprovalReason::Ddl,
        WriteReason::ExplainAnalyze => ApprovalReason::ExplainAnalyze,
        WriteReason::Other => ApprovalReason::Other,
        _ => ApprovalReason::Unknown,
    }
}

fn stage_of(stage: ApprovalStage) -> AiStage {
    match stage {
        ApprovalStage::ProdSecond => AiStage::ProdSecond,
        _ => AiStage::Confirm,
    }
}

/// The question the surface's approver gets for a host request.
fn approval_of(request: &WriteRequest) -> ExternalApproval {
    ExternalApproval::new(
        request.connection.id.clone(),
        request.sql.clone(),
        request.params.clone(),
        request.class,
        reason_of(request.reason),
        stage_of(request.stage),
    )
}

impl QueryApprover for SurfaceApprover {
    fn approve<'a>(&'a self, request: &'a WriteRequest) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            match self.ctx.approve(approval_of(request)).await {
                AiDecision::Approve => Decision::Approve,
                _ => Decision::Deny,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use sqler_ai_api::ToolCall;
    use sqler_ai_tools::{ConnectionInfo, Limits, SessionSource, ToolSet};
    use sqler_driver_api::sqler_core::{DriverId, Environment};
    use sqler_plugin_api::PluginId;
    use sqler_plugin_host::GuestError;
    use sqler_sql::StatementClass;
    use sqler_ui::connections::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt};
    use sqler_ui::plugins::grant::GrantBook;

    use super::super::settings::Prefs;
    use super::*;

    /// A connector that never connects.
    struct Offline;

    impl Connector for Offline {
        fn driver_ids(&self) -> Vec<DriverId> {
            Vec::new()
        }

        fn connect(
            &self,
            _request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            Box::pin(std::future::ready(Err(ConnectFailure::Aborted)))
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    fn offline_data() -> AppData {
        AppData::new(None, Arc::new(Offline))
    }

    fn guest(code: ErrorCode, message: &str) -> CallError {
        CallError::Guest(GuestError {
            code,
            message: message.to_owned(),
        })
    }

    #[test]
    fn guest_errors_map_to_the_tool_errors() {
        assert!(matches!(
            map_error(guest(ErrorCode::InvalidArgument, "bad")),
            ToolError::InvalidArguments(m) if m == "bad"
        ));
        assert!(matches!(
            map_error(guest(ErrorCode::NotFound, "gone")),
            ToolError::NotFound(_)
        ));
        assert!(matches!(
            map_error(guest(ErrorCode::CapabilityDenied, "no")),
            ToolError::NotAvailable(_)
        ));
        assert!(matches!(
            map_error(guest(ErrorCode::Declined, "no")),
            ToolError::Host(m) if m == "the plugin tool failed: no"
        ));
    }

    #[test]
    fn a_long_guest_message_is_clipped() {
        let ToolError::InvalidArguments(m) =
            map_error(guest(ErrorCode::InvalidArgument, &"x".repeat(2000)))
        else {
            panic!("wrong error");
        };
        assert_eq!(m.chars().count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn engine_failures_carry_no_engine_text() {
        for error in [
            CallError::Trap {
                message: "wasm backtrace secret".into(),
            },
            CallError::MemoryLimit { limit: 1 },
            CallError::HostPanic,
            CallError::Engine {
                message: "engine secret".into(),
            },
        ] {
            let ToolError::Host(m) = map_error(error) else {
                panic!("wrong error");
            };
            assert_eq!(m, "the plugin tool failed");
        }
        assert!(matches!(
            map_error(CallError::Timeout {
                after: Duration::from_secs(1)
            }),
            ToolError::Host(m) if m == "the plugin tool did not finish in time"
        ));
        assert!(matches!(
            map_error(CallError::Cancelled),
            ToolError::Host(m) if m == "the plugin tool was cancelled"
        ));
        for error in [
            CallError::Disabled,
            CallError::NotFound,
            CallError::MissingExport {
                interface: "ai-tools",
            },
        ] {
            assert!(matches!(map_error(error), ToolError::NotAvailable(_)));
        }
    }

    #[test]
    fn a_write_request_maps_to_the_surface_question() {
        let conn = PluginConnection::new(
            "c1",
            "Orders",
            DriverId::new("sqlite").unwrap(),
            Some(Environment::Prod),
        );
        let request = WriteRequest::new(
            "acme".parse::<PluginId>().unwrap(),
            "Acme",
            conn,
            "update t set a = 1",
            Vec::new(),
            StatementClass::Write,
            WriteReason::ExplainAnalyze,
            ApprovalStage::ProdSecond,
        );
        let ask = approval_of(&request);
        assert_eq!(ask.connection, "c1");
        assert_eq!(ask.sql, "update t set a = 1");
        assert_eq!(ask.reason, ApprovalReason::ExplainAnalyze);
        assert_eq!(ask.stage, AiStage::ProdSecond);
        assert_eq!(reason_of(WriteReason::Ddl), ApprovalReason::Ddl);
        assert_eq!(reason_of(WriteReason::Other), ApprovalReason::Other);
        assert_eq!(reason_of(WriteReason::Unknown), ApprovalReason::Unknown);
        assert_eq!(stage_of(ApprovalStage::Confirm), AiStage::Confirm);
    }

    /// A surface that lends one connection and no sessions.
    struct Lends;

    impl SessionSource for Lends {
        fn connections(&self) -> Vec<ConnectionInfo> {
            vec![
                ConnectionInfo::new("mine", "Mine", DriverId::new("sqlite").unwrap(), None)
                    .with_read_only(true),
            ]
        }

        fn catalog(&self, _connection: &str) -> Option<Catalog> {
            None
        }

        fn acquire<'a>(
            &'a self,
            _connection: &'a str,
        ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
            Box::pin(std::future::ready(Err(ToolError::NotAvailable(
                "none".into(),
            ))))
        }
    }

    /// An external tool that keeps the context of its call.
    #[derive(Default)]
    struct Keeps(Mutex<Option<ExternalContext>>);

    impl ExternalTools for Keeps {
        fn tools(&self) -> Vec<ExternalTool> {
            vec![ExternalTool::new(
                "keep__ctx",
                "keeps the context",
                serde_json::json!({"type": "object"}),
            )]
        }

        fn call<'a>(
            &'a self,
            _name: &'a str,
            _arguments: Value,
            ctx: ExternalContext,
        ) -> BoxFuture<'a, Result<Value, ToolError>> {
            *self.0.lock().unwrap() = Some(ctx);
            Box::pin(std::future::ready(Ok(serde_json::json!({}))))
        }
    }

    #[tokio::test]
    async fn surface_data_shows_only_the_surface_connections_and_refuses_foreign_ids() {
        let keeps = Arc::new(Keeps::default());
        let set =
            ToolSet::new(Arc::new(Lends), Limits::default()).with_external_tools(keeps.clone());
        let call = ToolCall::new("1", "keep__ctx", "{}");
        let _ = set.call(&call).await;
        let ctx = keeps.0.lock().unwrap().clone().unwrap();
        let data = SurfaceData {
            data: Arc::new(offline_data()),
            ctx,
        };
        let listed = data.connections();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "mine");
        // The surface's read-only flag is applied at the data layer.
        assert!(listed[0].read_only);
        assert!(matches!(
            data.acquire("foreign").await.err(),
            Some(DataError::UnknownConnection(_))
        ));
        assert!(data.catalog("foreign").is_none());
    }

    #[test]
    fn no_tools_and_no_engine_before_the_first_refresh() {
        let plugins = Arc::new(AppPlugins::new(
            Arc::new(GrantBook::in_memory()),
            Arc::new(Prefs::new(None)),
            None,
            Vec::new(),
            None,
        ));
        let tools = AppAiTools::new(Arc::clone(&plugins), Arc::new(offline_data()));
        assert_eq!(tools.tools().len(), 0);
        assert!(plugins.is_idle());
    }
}

/// The `ai-probe` guest through a real tool set: needs `SQLER_PLUGIN_FIXTURES` (see `backend.rs`).
#[cfg(all(test, feature = "driver-sqlite"))]
mod e2e {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use sqler_ai_api::ToolCall;
    use sqler_ai_tools::{
        ApprovalRequest, Approver, AuditEntry, ConnectionInfo, Limits, MemoryAudit, REDACTED,
        SessionSource, Surface, ToolSet, unwrap_untrusted,
    };
    use sqler_config::{ConfigDir, ConnectionSpec, ConnectionStore};
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query};
    use sqler_driver_api::{Driver, collect};
    use sqler_driver_sqlite::SqliteDriver;
    use sqler_plugin_api::{PluginId, SettingValue};
    use sqler_ui::connections::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt};
    use sqler_ui::plugins::PluginsBackend;
    use sqler_ui::plugins::grant::{GrantBook, GrantCommand};

    use super::super::scratch::Scratch;
    use super::super::settings::Prefs;
    use super::*;

    /// Opens every saved connection as a `SQLite` file.
    struct Files;

    impl Connector for Files {
        fn driver_ids(&self) -> Vec<DriverId> {
            Vec::new()
        }

        fn connect(
            &self,
            request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            let target = request.spec.target.clone();
            Box::pin(async move {
                let id = DriverId::new("sqlite").map_err(|_| ConnectFailure::Aborted)?;
                let config = ConnectionConfig::new("t", id, target);
                Ok(SqliteDriver::new().connect(&config).await?)
            })
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    /// The surface: lends the one connection, never a session of its own.
    struct Lent(String);

    impl SessionSource for Lent {
        fn connections(&self) -> Vec<ConnectionInfo> {
            vec![ConnectionInfo::new(
                self.0.clone(),
                "Notes",
                DriverId::new("sqlite").unwrap(),
                None,
            )]
        }

        fn catalog(&self, _connection: &str) -> Option<Catalog> {
            None
        }

        fn acquire<'a>(
            &'a self,
            _connection: &'a str,
        ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
            Box::pin(std::future::ready(Err(ToolError::NotAvailable(
                "none".into(),
            ))))
        }
    }

    /// Approves everything and counts the asks.
    #[derive(Default)]
    struct Yes(AtomicUsize);

    impl Approver for Yes {
        fn approve<'a>(&'a self, _request: &'a ApprovalRequest) -> BoxFuture<'a, AiDecision> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(AiDecision::Approve))
        }
    }

    fn fixture() -> Option<PathBuf> {
        let required = std::env::var("SQLER_TEST_REQUIRE_PLUGINS").is_ok_and(|v| v == "1");
        let dir = std::env::var_os("SQLER_PLUGIN_FIXTURES").map(PathBuf::from);
        let probe = dir.map(|d| {
            let d = if d.is_absolute() {
                d
            } else {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join(d)
            };
            d.join("ai-probe")
        });
        let found = probe.filter(|p| p.join("sqler.toml").is_file());
        assert!(
            found.is_some() || !required,
            "SQLER_TEST_REQUIRE_PLUGINS=1 but the ai-probe fixture is not staged"
        );
        found
    }

    fn content(report: &sqler_ai_tools::CallReport) -> Value {
        serde_json::from_str(unwrap_untrusted(&report.result.content).unwrap()).unwrap()
    }

    fn only(audit: &MemoryAudit, tool: &str) -> AuditEntry {
        let mut rows: Vec<AuditEntry> = audit
            .entries()
            .into_iter()
            .filter(|e| e.tool == tool)
            .collect();
        assert_eq!(rows.len(), 1, "one audit row for {tool}");
        rows.remove(0)
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one scenario, top to bottom
    fn the_ai_probe_tools_run_through_the_tool_set() {
        let Some(probe) = fixture() else {
            eprintln!(
                "skipped: set SQLER_PLUGIN_FIXTURES=target/plugins/<profile> to run this test"
            );
            return;
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let s = Scratch::new("ai-e2e");
        let config = ConfigDir::new(s.path());
        let db = s.path().join("notes.db");
        let mut spec = ConnectionSpec::new(
            "Notes",
            DriverId::new("sqlite").unwrap(),
            db.to_str().unwrap(),
        );
        spec.environment = None;
        let mut store = ConnectionStore::new();
        let conn = store.add_connection(spec, None).unwrap();
        config.save_connection_store(&store).unwrap();
        let conn = conn.as_str().to_owned();
        rt.block_on(async {
            let cfg =
                ConnectionConfig::new("t", DriverId::new("sqlite").unwrap(), db.to_str().unwrap());
            let mut session = SqliteDriver::new().connect(&cfg).await.unwrap();
            for sql in [
                "create table notes (id integer, password text)",
                "insert into notes values (1, 'CANARY-PW')",
            ] {
                let query = Query::new(sql);
                let mut stream = session.execute(&query).await.unwrap();
                collect(&mut *stream).await.unwrap();
            }
            session.close().await.unwrap();
        });

        let grants = Arc::new(GrantBook::open(config.clone()));
        let backend = Arc::new(AppPlugins::new(
            Arc::clone(&grants),
            Arc::new(Prefs::new(Some(config.clone()))),
            Some(&config),
            vec![probe],
            None,
        ));
        let data = Arc::new(AppData::new(Some(config.clone()), Arc::new(Files)));
        let external = Arc::new(AppAiTools::new(Arc::clone(&backend), data));
        assert!(
            external.tools().is_empty(),
            "nothing is listed before a refresh"
        );

        let id: PluginId = "ai-probe".parse().unwrap();
        let snap = backend.refresh();
        assert_eq!(snap.rows.len(), 1, "{:?}", snap.problems);
        assert!(
            external.tools().is_empty(),
            "no grant, no tools: {:?}",
            external.tools()
        );
        let manifest = Arc::clone(&snap.rows[0].manifest);
        let prompt = grants
            .prompt(&manifest, snap.rows[0].component.as_ref())
            .unwrap();
        grants.decide(&prompt, GrantCommand::Approve).unwrap();
        backend.grants_changed(&id);
        let names: Vec<String> = external.tools().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"ai-probe__echo".to_owned()), "{names:?}");

        backend
            .set_setting(&id, "connection", SettingValue::Text(conn.clone()))
            .unwrap();
        let set_sql = |sql: &str| {
            backend
                .set_setting(&id, "sql", SettingValue::Text(sql.to_owned()))
                .unwrap();
        };
        let audit = Arc::new(MemoryAudit::new());
        let yes = Arc::new(Yes::default());
        let base = ToolSet::new(Arc::new(Lent(conn.clone())), Limits::default())
            .with_external_tools(external);
        let plain = base.with_audit(audit.clone(), Surface::Mcp);
        let approving = base
            .with_approver(yes.clone())
            .with_audit(audit.clone(), Surface::Mcp);
        let run = |ts: &ToolSet, name: &str, args: Value| {
            rt.block_on(ts.call_detailed(&ToolCall::new("1", name, args.to_string())))
        };

        // Listed, called, audited.
        assert!(
            plain
                .definitions()
                .iter()
                .any(|d| d.name == "ai-probe__echo")
        );
        let report = run(&plain, "ai-probe__echo", json!({"text": "hi"}));
        assert!(!report.result.is_error);
        assert_eq!(content(&report)["text"], "hi");
        assert_eq!(only(&audit, "ai-probe__echo").surface, Surface::Mcp);

        // A read runs without an approver; the column named like a secret is masked.
        set_sql("select id, password from notes");
        let report = run(&plain, "ai-probe__query", json!({}));
        let text = report.result.content.clone();
        assert!(!report.result.is_error, "{text}");
        assert!(!text.contains("CANARY-PW"), "{text}");
        assert!(text.contains(REDACTED), "{text}");

        // A write: refused with no approver, run with an approving one.
        set_sql("insert into notes values (2, 'x')");
        let report = run(&plain, "ai-probe__query", json!({}));
        assert!(report.result.is_error);
        assert_eq!(content(&report)["error"]["code"], "no_approver");
        assert_eq!(yes.0.load(Ordering::SeqCst), 0);
        let report = run(&approving, "ai-probe__query", json!({}));
        assert!(!report.result.is_error, "{}", report.result.content);
        assert_eq!(yes.0.load(Ordering::SeqCst), 1);
        assert_eq!(report.executed.len(), 1);

        // A trap is an error result, with no engine text.
        let report = run(&plain, "ai-probe__trap", json!({}));
        assert!(report.result.is_error);
        assert_eq!(content(&report)["error"]["code"], "host");
        assert!(!report.result.content.contains("backtrace"));
    }
}
