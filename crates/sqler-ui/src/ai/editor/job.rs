// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! One assist request as a future: build the provider, run T9's `Assist` over a catalog-only
//! tool set, race it against a cancel signal and turn the answer into an [`AssistOutcome`].

use std::sync::Arc;

use sqler_ai_tools::{
    Assist, AssistError, ConnectionInfo, Conversation, FixProposal, Surface, ToolSet, unified_diff,
};
use sqler_catalog::Catalog;
use sqler_config::{AiConfig, ProviderConfig, contains_redaction};

use super::{AssistOutcome, AssistRequest, CANCELLED, EditorSource};
use crate::ai::{AiAudit, AiProviderFactory};
use crate::tabs::TabId;

/// Everything a request needs, owned so it can move to tokio. `Debug` prints the id, the tab and
/// the kind only: never the question, the SQL, the catalog or the provider.
pub struct AssistJob {
    /// The request id; the workspace drops answers of other ids.
    pub id: u64,
    /// The tab that asked.
    pub tab: TabId,
    /// What to do.
    pub request: AssistRequest,
    /// The provider to build.
    pub provider: ProviderConfig,
    /// The model id.
    pub model: String,
    /// What the model may know about the connection.
    pub info: ConnectionInfo,
    /// The catalog the browser loaded, if it is ready.
    pub catalog: Option<Catalog>,
    /// The limits and the redaction list.
    pub config: AiConfig,
    /// Where the tool calls are audited (required).
    pub audit: AiAudit,
}

impl std::fmt::Debug for AssistJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistJob")
            .field("id", &self.id)
            .field("tab", &self.tab)
            .field("kind", &self.request.kind())
            .finish_non_exhaustive()
    }
}

fn describe(error: &AssistError) -> String {
    error.to_string()
}

/// Runs the request. Dropping the future (or firing `cancel`, or dropping its sender) ends the
/// provider stream; the result is then `Err(CANCELLED)`.
///
/// # Errors
/// The text of the failure: a provider that cannot be built, a provider error, a timeout, an
/// answer without SQL. It never carries a key.
pub async fn run_assist(
    job: AssistJob,
    factory: Arc<dyn AiProviderFactory>,
    cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<AssistOutcome, String> {
    let work = work(job, factory);
    tokio::select! {
        biased;
        _ = cancel => Err(CANCELLED.to_owned()),
        result = work => result,
    }
}

async fn work(
    job: AssistJob,
    factory: Arc<dyn AiProviderFactory>,
) -> Result<AssistOutcome, String> {
    let AssistJob {
        request,
        provider,
        model,
        info,
        catalog,
        config,
        audit,
        ..
    } = job;
    let provider = tokio::task::spawn_blocking(move || factory.build(&provider, None))
        .await
        .map_err(|_| "The AI provider could not be started.".to_owned())?
        .map_err(|e| e.to_string())?;
    let connection = info.id.clone();
    let tools = ToolSet::configured(Arc::new(EditorSource::new(info, catalog)), &config);
    let tools = audit.attach(tools, Surface::Assist);
    let assist = Assist::new(tools, provider, model);
    let mut conv = Conversation::new();
    match request {
        AssistRequest::Generate { question } => {
            let answer = assist
                .nl_to_sql(&connection, &question, &mut conv)
                .await
                .map_err(|e| describe(&e))?;
            Ok(AssistOutcome::Sql {
                sql: answer.sql,
                rationale: answer.rationale,
                tables: answer.tables.into_iter().map(|t| t.name).collect(),
                applicable: answer.applicable,
                inserted: false,
            })
        }
        AssistRequest::Explain { sql } => {
            let explanation = assist
                .explain(&connection, &sql, false, &mut conv)
                .await
                .map_err(|e| describe(&e))?;
            Ok(AssistOutcome::Explanation {
                sql,
                text: explanation.text,
            })
        }
        AssistRequest::Fix {
            sql,
            error,
            statement,
        } => {
            let fix = assist
                .fix(&connection, &sql, &error, &mut conv)
                .await
                .map_err(|e| describe(&e))?;
            Ok(fix_outcome(statement, fix))
        }
    }
}

/// Turns a fix proposal into an outcome. The failed statement is held without its
/// `;`, so a model's trailing `;` is dropped when the original has none (else
/// Accept would leave `;;`); the diff and the two flags are then recomputed.
fn fix_outcome(statement: usize, fix: FixProposal) -> AssistOutcome {
    let mut sql = fix.sql;
    let (diff, applicable, unchanged) = if fix.original.trim_end().ends_with(';') {
        (fix.diff, fix.applicable, fix.unchanged)
    } else {
        sql = sql.trim_end().trim_end_matches(';').trim_end().to_owned();
        let diff = unified_diff(fix.original.trim(), &sql, 3);
        let unchanged = diff.is_empty();
        let applicable = !unchanged && !contains_redaction(&sql);
        (diff, applicable, unchanged)
    };
    AssistOutcome::Fix {
        statement,
        original: fix.original,
        sql,
        diff,
        rationale: fix.rationale,
        applicable,
        unchanged,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_ai_api::mock::ScriptedProvider;
    use sqler_ai_api::{
        AiError, AiProvider, BoxFuture, Capabilities, ChatEvent, ChatRequest, ChatStream,
        ModelInfo, StopReason,
    };
    use sqler_ai_tools::{MemoryAudit, UNTRUSTED_OPEN};

    use crate::ai::AiAudit;
    use sqler_config::{ProviderId, ProviderKind, Secret};
    use sqler_driver_api::sqler_core::{
        ColumnInfo, DataType, DbObject, DriverId, ObjectKind, ObjectPath, Table,
    };

    use super::*;
    use crate::browser::testing::FakeIntrospect;

    /// A factory that hands out one provider.
    pub(crate) struct OneProvider(pub(crate) Arc<dyn AiProvider>);

    impl AiProviderFactory for OneProvider {
        fn kinds(&self) -> Vec<ProviderKind> {
            vec![ProviderKind::Ollama]
        }

        fn build(
            &self,
            _provider: &ProviderConfig,
            _key: Option<&Secret>,
        ) -> Result<Arc<dyn AiProvider>, AiError> {
            Ok(Arc::clone(&self.0))
        }
    }

    pub(crate) fn answer(text: &str) -> Vec<Result<ChatEvent, AiError>> {
        vec![
            Ok(ChatEvent::TextDelta(text.to_owned())),
            Ok(ChatEvent::Done {
                stop: StopReason::EndTurn,
            }),
        ]
    }

    pub(crate) fn scripted(answers: &[&str]) -> Arc<ScriptedProvider> {
        let p = ScriptedProvider::new("mock");
        for a in answers {
            p.push_script(answer(a));
        }
        Arc::new(p)
    }

    pub(crate) fn info() -> ConnectionInfo {
        ConnectionInfo::new("dev", "Dev", DriverId::new("sqlite").unwrap(), None)
    }

    pub(crate) fn a_tab() -> TabId {
        use crate::tabs::{TabConnection, TabKind, TabStrip};
        TabStrip::default().open(
            TabConnection {
                name: "Dev".into(),
                driver: DriverId::new("sqlite").unwrap(),
                color: None,
                environment: None,
            },
            TabKind::Query,
        )
    }

    pub(crate) fn job(request: AssistRequest, catalog: Option<Catalog>) -> AssistJob {
        AssistJob {
            id: 1,
            tab: a_tab(),
            request,
            provider: ProviderConfig::new(ProviderId::new("local").unwrap(), ProviderKind::Ollama),
            model: "m".into(),
            info: info(),
            catalog,
            config: AiConfig::default(),
            audit: AiAudit::new(Arc::new(MemoryAudit::new())),
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// Runs a job to its end with a cancel sender that is kept alive.
    pub(crate) fn run(
        rt: &tokio::runtime::Runtime,
        job: AssistJob,
        provider: Arc<dyn AiProvider>,
    ) -> Result<AssistOutcome, String> {
        let (_keep, rx) = tokio::sync::oneshot::channel();
        rt.block_on(run_assist(job, Arc::new(OneProvider(provider)), rx))
    }

    #[test]
    fn generate_gives_sql_and_rationale() {
        let rt = rt();
        let p = scripted(&["```sql\nselect 1\n```\nA constant."]);
        let out = run(
            &rt,
            job(
                AssistRequest::Generate {
                    question: "one".into(),
                },
                None,
            ),
            p,
        )
        .unwrap();
        let AssistOutcome::Sql {
            sql,
            rationale,
            inserted,
            ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert_eq!(sql, "select 1");
        assert_eq!(rationale, "A constant.");
        assert!(!inserted);
    }

    #[test]
    fn explain_gives_its_text_and_sends_no_plan() {
        let rt = rt();
        let p = scripted(&["It reads one row."]);
        let out = run(
            &rt,
            job(
                AssistRequest::Explain {
                    sql: "select 1".into(),
                },
                None,
            ),
            p.clone(),
        )
        .unwrap();
        assert_eq!(
            out,
            AssistOutcome::Explanation {
                sql: "select 1".into(),
                text: "It reads one row.".into()
            }
        );
        let sent = format!("{:?}", p.requests());
        assert!(!sent.contains("Query plan"));
    }

    fn fix_job() -> AssistJob {
        job(
            AssistRequest::Fix {
                sql: "select nme from customers".into(),
                error: "no such column: nme".into(),
                statement: 2,
            },
            None,
        )
    }

    #[test]
    fn fix_gives_a_diff_and_is_applicable() {
        let rt = rt();
        let p = scripted(&["```sql\nselect name from customers\n```\nThe column is name."]);
        let out = run(&rt, fix_job(), p).unwrap();
        let AssistOutcome::Fix {
            statement,
            sql,
            diff,
            applicable,
            unchanged,
            ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert_eq!(statement, 2);
        assert_eq!(sql, "select name from customers");
        assert!(diff.contains("+select name from customers"), "{diff}");
        assert!(applicable && !unchanged);
    }

    #[test]
    fn a_trailing_semicolon_is_stripped_when_the_original_has_none() {
        let rt = rt();
        let p = scripted(&["```sql\nselect name from customers;\n```\nThe column is name."]);
        let out = run(&rt, fix_job(), p).unwrap();
        let AssistOutcome::Fix {
            sql,
            diff,
            applicable,
            unchanged,
            ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert_eq!(sql, "select name from customers");
        assert!(diff.contains("+select name from customers"), "{diff}");
        assert!(!diff.contains(';'), "{diff}");
        assert!(applicable && !unchanged);
    }

    #[test]
    fn an_answer_that_only_adds_a_semicolon_is_not_applicable() {
        let rt = rt();
        let p = scripted(&["```sql\nselect nme from customers;\n```\nIt was fine."]);
        let out = run(&rt, fix_job(), p).unwrap();
        let AssistOutcome::Fix {
            sql,
            applicable,
            unchanged,
            ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert_eq!(sql, "select nme from customers");
        assert!(unchanged && !applicable);
    }

    #[test]
    fn a_semicolon_is_kept_when_the_original_has_one() {
        let rt = rt();
        let job = job(
            AssistRequest::Fix {
                sql: "select nme from customers;".into(),
                error: "no such column: nme".into(),
                statement: 1,
            },
            None,
        );
        let p = scripted(&["```sql\nselect name from customers;\n```\nThe column is name."]);
        let out = run(&rt, job, p).unwrap();
        let AssistOutcome::Fix {
            sql, applicable, ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert_eq!(sql, "select name from customers;");
        assert!(applicable);
    }

    #[test]
    fn a_fix_with_a_redaction_placeholder_is_not_applicable() {
        let rt = rt();
        let p = scripted(&[
            "```sql\nselect * from customers where token = '<redacted>'\n```\nUse the token.",
        ]);
        let out = run(&rt, fix_job(), p).unwrap();
        let AssistOutcome::Fix { applicable, .. } = out else {
            panic!("{out:?}");
        };
        assert!(!applicable);
    }

    #[test]
    fn the_catalog_reaches_the_prompt_in_an_envelope_and_the_audit() {
        let rt = rt();
        let customers = DbObject::new(ObjectKind::Table, ObjectPath::default(), "customers");
        let intro = FakeIntrospect::new()
            .with_objects(
                &ObjectPath::default(),
                ObjectKind::Table,
                vec![customers.clone()],
            )
            .with_table(
                Table::new(customers).with_columns(vec![ColumnInfo::new("id", DataType::Integer)]),
            );
        let catalog = rt.block_on(async {
            let (catalog, worker) = Catalog::new(intro.boxed());
            tokio::spawn(worker.run());
            catalog
        });
        let p = scripted(&["```sql\nselect id from customers\n```\nIds."]);
        let sink = Arc::new(MemoryAudit::new());
        let mut j = job(
            AssistRequest::Generate {
                question: "list customers".into(),
            },
            Some(catalog),
        );
        j.audit = AiAudit::new(sink.clone());
        let out = run(&rt, j, p.clone()).unwrap();
        assert!(matches!(out, AssistOutcome::Sql { .. }));
        let requests = p.requests();
        let user = requests[0].messages.last().unwrap().text();
        let open = user.find(UNTRUSTED_OPEN).expect("an envelope");
        assert!(user[open..].contains("customers"), "{user}");
        assert!(!user[..open].contains("customers"), "{user}");
        let entries = sink.entries();
        assert_ne!(entries.len(), 0);
        assert!(entries.iter().all(|e| e.surface == Surface::Assist));
    }

    /// A provider whose stream never ends; it counts the open streams.
    #[derive(Default)]
    struct Stall(Arc<AtomicUsize>);

    struct StallStream(Arc<AtomicUsize>);

    impl ChatStream for StallStream {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<ChatEvent, AiError>>> {
            Box::pin(std::future::pending())
        }
    }

    impl Drop for StallStream {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl AiProvider for Stall {
        fn id(&self) -> &'static str {
            "stall"
        }

        fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
            Box::pin(std::future::ready(Ok(Vec::new())))
        }

        fn stream(&self, _request: ChatRequest) -> Box<dyn ChatStream + '_> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::new(StallStream(Arc::clone(&self.0)))
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
    }

    fn cancelled(drop_sender: bool) {
        let rt = rt();
        let stall = Arc::new(Stall::default());
        let open = Arc::clone(&stall.0);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = rt.spawn(run_assist(
            job(
                AssistRequest::Explain {
                    sql: "select 1".into(),
                },
                None,
            ),
            Arc::new(OneProvider(stall)),
            rx,
        ));
        // Wait (bounded) until the stream is open, then cancel.
        rt.block_on(async {
            for _ in 0..200 {
                if open.load(Ordering::SeqCst) == 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        assert_eq!(open.load(Ordering::SeqCst), 1);
        if drop_sender {
            drop(tx);
        } else {
            tx.send(()).unwrap();
        }
        let result = rt.block_on(handle).unwrap();
        assert_eq!(result, Err(CANCELLED.to_owned()));
        assert_eq!(open.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancel_ends_the_stream() {
        cancelled(false);
    }

    #[test]
    fn a_dropped_sender_also_cancels() {
        cancelled(true);
    }

    #[test]
    fn a_missing_provider_is_an_error_text() {
        let rt = rt();
        let (_keep, rx) = tokio::sync::oneshot::channel();
        let err = rt
            .block_on(run_assist(
                job(
                    AssistRequest::Explain {
                        sql: "select 1".into(),
                    },
                    None,
                ),
                Arc::new(crate::ai::NoProviders),
                rx,
            ))
            .unwrap_err();
        assert!(err.contains("not available"), "{err}");
    }

    #[test]
    fn debug_shows_id_tab_and_kind_only() {
        let j = job(
            AssistRequest::Generate {
                question: "CANARY-QUESTION".into(),
            },
            None,
        );
        let text = format!("{j:?}");
        assert!(text.contains("Generate"), "{text}");
        assert!(!text.contains("CANARY"), "{text}");
    }
}
