// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The async jobs behind the object context menu.
//!
//! The workspace runs these on tokio through `SessionBridge`. Jobs that need SQL execution or
//! `Introspect::definition` open a short-lived session through the [`Connector`] and close it on
//! every path (success, error, cancel). Jobs that only need the model use the [`Catalog`].

use std::slice;
use std::sync::Arc;

use sqler_catalog::{Catalog, NodeKey};
use sqler_driver_api::sqler_core::{DbObject, Listing, ObjectKind, Query};
use sqler_driver_api::{
    BoxFuture, CancelFlag, DriverError, QueryEvent, QueryStream, Session, collect,
};
use sqler_sql::{
    DEFAULT_ROW_LIMIT, DdlError, Dialect, Script, create_script, create_table, drop_script,
    insert_template, select_star, supports,
};

use super::connect::{ConnectRequest, Connector};
use super::error::BrowserError;
use super::tab::{Property, properties};
use super::tree::is_table_like;
use crate::grid::{GridSink, ingest};

async fn close_session(session: Box<dyn Session>) {
    if let Err(err) = session.close().await {
        eprintln!("sqler: failed to close a session: {err}");
    }
}

/// A stream that fails at once; it tells a grid sink why no rows will come.
struct FailedStream(Option<DriverError>);

impl QueryStream for FailedStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        Box::pin(std::future::ready(self.0.take().map(Err)))
    }
}

/// Tells the grid behind `sink` that the load failed, so its tab does not wait forever.
async fn fail_sink(sink: GridSink, error: DriverError) {
    let mut stream = FailedStream(Some(error));
    // The sink gets the failure; the same error is returned to the caller separately.
    let _ = ingest(&mut stream, sink).await;
}

/// Reads the definition of each object on a new session, which is closed afterwards.
async fn definitions(
    connector: &Arc<dyn Connector>,
    request: ConnectRequest,
    cancel: &CancelFlag,
    objects: &[DbObject],
) -> Result<Vec<Option<String>>, BrowserError> {
    if cancel.is_cancelled() {
        return Err(BrowserError::Cancelled);
    }
    let mut session = connector.connect(request).await?;
    let result = read_definitions(&mut *session, cancel, objects).await;
    close_session(session).await;
    result
}

async fn read_definitions(
    session: &mut (dyn Session + '_),
    cancel: &CancelFlag,
    objects: &[DbObject],
) -> Result<Vec<Option<String>>, BrowserError> {
    let Some(introspect) = session.introspection() else {
        return Err(BrowserError::NoIntrospection);
    };
    let mut out = Vec::with_capacity(objects.len());
    for object in objects {
        if cancel.is_cancelled() {
            return Err(BrowserError::Cancelled);
        }
        out.push(introspect.definition(object).await?);
    }
    Ok(out)
}

/// Generates one script for `obj`.
///
/// Only `Create` can connect: it opens a short-lived session to read definitions, and only when
/// the kind needs them. `SelectStar`, `Drop` and `InsertTemplate` never connect, and neither does
/// `Create` of a schema, database, extension or role.
///
/// # Errors
/// [`BrowserError::Ddl`] when the dialect has no such script, [`BrowserError::NotFound`] when the
/// table is gone, [`BrowserError::Connect`], [`BrowserError::Catalog`] or
/// [`BrowserError::Driver`] when loading fails, and [`BrowserError::Cancelled`] when `cancel` is set.
pub async fn generate_script(
    connector: Arc<dyn Connector>,
    request: ConnectRequest,
    catalog: Catalog,
    dialect: Dialect,
    obj: DbObject,
    script: Script,
    cancel: CancelFlag,
) -> Result<String, BrowserError> {
    if cancel.is_cancelled() {
        return Err(BrowserError::Cancelled);
    }
    if !supports(dialect, obj.kind, script) {
        return Err(DdlError::Unsupported {
            dialect,
            kind: obj.kind,
            script,
        }
        .into());
    }
    match script {
        Script::SelectStar => Ok(select_star(dialect, &obj, DEFAULT_ROW_LIMIT)?),
        Script::Drop => Ok(drop_script(dialect, &obj)?),
        Script::InsertTemplate => {
            let table = catalog.table(obj).await?.ok_or(BrowserError::NotFound)?;
            Ok(insert_template(dialect, &table)?)
        }
        Script::Create => create(&connector, request, &catalog, dialect, obj, &cancel).await,
        other => Err(DdlError::Unsupported {
            dialect,
            kind: obj.kind,
            script: other,
        }
        .into()),
    }
}

async fn create(
    connector: &Arc<dyn Connector>,
    request: ConnectRequest,
    catalog: &Catalog,
    dialect: Dialect,
    obj: DbObject,
    cancel: &CancelFlag,
) -> Result<String, BrowserError> {
    match obj.kind {
        ObjectKind::Table => {
            let table = catalog
                .table(obj.clone())
                .await?
                .ok_or(BrowserError::NotFound)?;
            let listing = catalog
                .children(NodeKey::new(obj.child_path(), ObjectKind::Index))
                .await?;
            let indexes: &[DbObject] = match &*listing {
                Listing::Objects(indexes) => indexes,
                Listing::Unsupported => &[],
            };
            let texts = if indexes.is_empty() {
                Vec::new()
            } else {
                definitions(connector, request, cancel, indexes).await?
            };
            let texts: Vec<String> = texts.into_iter().flatten().collect();
            Ok(create_table(dialect, &table, &texts)?)
        }
        ObjectKind::Schema | ObjectKind::Database | ObjectKind::Extension | ObjectKind::Role => {
            Ok(create_script(dialect, &obj, None)?)
        }
        _ => {
            let texts = definitions(connector, request, cancel, slice::from_ref(&obj)).await?;
            Ok(create_script(
                dialect,
                &obj,
                texts.first().and_then(Option::as_deref),
            )?)
        }
    }
}

/// Loads the fields to show for `obj`; tables, views and materialized views include their
/// columns and keys.
///
/// # Errors
/// [`BrowserError::Catalog`] when loading fails, [`BrowserError::NotFound`] when the table is gone.
pub async fn load_properties(
    catalog: Catalog,
    obj: DbObject,
) -> Result<Vec<Property>, BrowserError> {
    if is_table_like(obj.kind) {
        let table = catalog
            .table(obj.clone())
            .await?
            .ok_or(BrowserError::NotFound)?;
        Ok(properties(&obj, Some(&table)))
    } else {
        Ok(properties(&obj, None))
    }
}

/// Runs `sql` (the `SELECT *` text) on a short-lived session and streams the rows into `sink`.
///
/// A failure before the first row is also reported to the sink, so the grid shows it.
///
/// The grid model is bounded (see `GridLimits`): when its page fills up before the result ends,
/// this future stays pending, with the short-lived session still open, until the user fetches
/// more rows, cancels (`GridCancel`, `GridModel::cancel`) or drops the model; it then finishes
/// and closes the session.
///
/// # Errors
/// [`BrowserError::Connect`] or [`BrowserError::Driver`]; [`BrowserError::Cancelled`] when the
/// sink was cancelled.
pub async fn open_data(
    connector: Arc<dyn Connector>,
    request: ConnectRequest,
    sql: String,
    sink: GridSink,
) -> Result<(), BrowserError> {
    if sink.is_cancelled() {
        fail_sink(sink, DriverError::Cancelled).await;
        return Err(BrowserError::Cancelled);
    }
    let mut session = match connector.connect(request).await {
        Ok(session) => session,
        Err(err) => {
            fail_sink(sink, DriverError::Connection(err.to_string())).await;
            return Err(err.into());
        }
    };
    let query = Query::new(sql);
    let result = match session.execute(&query).await {
        Ok(mut stream) => ingest(&mut *stream, sink).await.map(|_| ()),
        Err(err) => {
            fail_sink(sink, copy_of(&err)).await;
            Err(err)
        }
    };
    close_session(session).await;
    result.map_err(BrowserError::from)
}

/// A copy of `err` with the same message, for the sink (`DriverError` is not `Clone`).
fn copy_of(err: &DriverError) -> DriverError {
    match err {
        DriverError::Cancelled => DriverError::Cancelled,
        DriverError::Connection(text) => DriverError::Connection(text.clone()),
        DriverError::Query(text) => DriverError::Query(text.clone()),
        other => DriverError::Query(other.to_string()),
    }
}

/// Runs `sql` (the `DROP` text) on a short-lived session and waits for it to finish.
///
/// The statement is executed exactly as given.
///
/// # Errors
/// [`BrowserError::Connect`] or [`BrowserError::Driver`].
pub async fn run_drop(
    connector: Arc<dyn Connector>,
    request: ConnectRequest,
    sql: String,
) -> Result<(), BrowserError> {
    let mut session = connector.connect(request).await?;
    let query = Query::new(sql);
    let result = match session.execute(&query).await {
        Ok(mut stream) => collect(&mut *stream).await.map(|_| ()),
        Err(err) => Err(err),
    };
    close_session(session).await;
    result.map_err(BrowserError::from)
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::{Duration, Instant};

    use sqler_driver_api::sqler_core::{ColumnInfo, DataType, ObjectPath, PrimaryKey, Table};
    use tokio::runtime::{Builder, Runtime};

    use super::*;
    use crate::browser::testing::{FakeIntrospect, SessionConnector, request};
    use crate::grid::{GridLimits, IngestState, grid_channel, grid_channel_with};

    fn runtime() -> Runtime {
        Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap()
    }

    fn schema_path() -> ObjectPath {
        ObjectPath::root().in_schema("public")
    }

    fn table_obj() -> DbObject {
        DbObject::new(ObjectKind::Table, schema_path(), "users")
    }

    fn index_obj() -> DbObject {
        DbObject::new(ObjectKind::Index, table_obj().child_path(), "users_ix")
    }

    fn model() -> Table {
        let mut t = Table::new(table_obj());
        t.columns = vec![
            ColumnInfo::new("id", DataType::Integer).with_nullable(false),
            ColumnInfo::new("name", DataType::Text).with_native_type("text"),
        ];
        t.primary_key = Some(PrimaryKey::new(vec!["id".into()]));
        t
    }

    fn intro() -> FakeIntrospect {
        FakeIntrospect::new()
            .with_table(model())
            .with_objects(
                &table_obj().child_path(),
                ObjectKind::Index,
                vec![index_obj()],
            )
            .with_definition(
                &index_obj(),
                "CREATE INDEX users_ix ON public.users USING btree (name)",
            )
            .with_definition(
                &DbObject::new(ObjectKind::View, schema_path(), "v"),
                "SELECT 1",
            )
    }

    /// A catalog over `intro` whose worker runs on `rt`.
    fn catalog(rt: &Runtime, intro: &FakeIntrospect) -> Catalog {
        let (catalog, worker) = Catalog::new(intro.boxed());
        rt.spawn(worker.run());
        catalog
    }

    struct Setup {
        rt: Runtime,
        connector: SessionConnector,
        catalog: Catalog,
    }

    fn setup() -> Setup {
        let rt = runtime();
        let intro = intro();
        let catalog = catalog(&rt, &intro);
        Setup {
            connector: SessionConnector::new(intro),
            rt,
            catalog,
        }
    }

    impl Setup {
        fn script(&self, obj: DbObject, script: Script) -> Result<String, BrowserError> {
            self.script_with(&self.connector, obj, script, CancelFlag::new())
        }

        fn script_with(
            &self,
            connector: &SessionConnector,
            obj: DbObject,
            script: Script,
            cancel: CancelFlag,
        ) -> Result<String, BrowserError> {
            self.rt.block_on(generate_script(
                Arc::new(connector.clone()),
                request(),
                self.catalog.clone(),
                Dialect::Postgres,
                obj,
                script,
                cancel,
            ))
        }
    }

    fn view_obj() -> DbObject {
        DbObject::new(ObjectKind::View, schema_path(), "v")
    }

    #[test]
    fn select_drop_and_insert_never_connect() {
        let s = setup();
        let select = s.script(table_obj(), Script::SelectStar).unwrap();
        assert_eq!(
            select,
            select_star(Dialect::Postgres, &table_obj(), DEFAULT_ROW_LIMIT).unwrap()
        );
        let drop = s.script(table_obj(), Script::Drop).unwrap();
        assert_eq!(drop, drop_script(Dialect::Postgres, &table_obj()).unwrap());
        let insert = s.script(table_obj(), Script::InsertTemplate).unwrap();
        assert_eq!(
            insert,
            insert_template(Dialect::Postgres, &model()).unwrap()
        );
        assert_eq!(s.connector.connects(), 0);
    }

    #[test]
    fn schema_create_needs_no_session() {
        let s = setup();
        let schema = DbObject::new(ObjectKind::Schema, ObjectPath::root(), "app");
        let sql = s.script(schema, Script::Create).unwrap();
        assert_eq!(sql, "CREATE SCHEMA \"app\";\n");
        assert_eq!(s.connector.connects(), 0);
    }

    #[test]
    fn table_create_includes_the_index_definitions() {
        let s = setup();
        let sql = s.script(table_obj(), Script::Create).unwrap();
        assert!(
            sql.starts_with("CREATE TABLE \"public\".\"users\" ("),
            "{sql}"
        );
        assert!(sql.contains("PRIMARY KEY (\"id\")"), "{sql}");
        assert!(sql.contains("CREATE INDEX users_ix"), "{sql}");
        assert_eq!(s.connector.intro.definition_calls(), vec![index_obj()]);
        assert_eq!((s.connector.connects(), s.connector.closed()), (1, 1));
    }

    #[test]
    fn table_without_indexes_does_not_connect() {
        let rt = runtime();
        let intro = FakeIntrospect::new().with_table(model());
        let catalog = catalog(&rt, &intro);
        let connector = SessionConnector::new(intro);
        let sql = rt
            .block_on(generate_script(
                Arc::new(connector.clone()),
                request(),
                catalog,
                Dialect::Postgres,
                table_obj(),
                Script::Create,
                CancelFlag::new(),
            ))
            .unwrap();
        assert!(sql.starts_with("CREATE TABLE"));
        assert_eq!(connector.connects(), 0);
    }

    #[test]
    fn view_create_uses_the_definition_and_closes_the_session() {
        let s = setup();
        let sql = s.script(view_obj(), Script::Create).unwrap();
        assert_eq!(sql, "CREATE VIEW \"public\".\"v\" AS\nSELECT 1;\n");
        assert_eq!((s.connector.connects(), s.connector.closed()), (1, 1));
    }

    #[test]
    fn missing_table_is_not_found() {
        let s = setup();
        let gone = DbObject::new(ObjectKind::Table, schema_path(), "gone");
        assert!(matches!(
            s.script(gone.clone(), Script::Create),
            Err(BrowserError::NotFound)
        ));
        assert!(matches!(
            s.script(gone, Script::InsertTemplate),
            Err(BrowserError::NotFound)
        ));
    }

    #[test]
    fn unsupported_script_is_a_ddl_error_without_connecting() {
        let s = setup();
        let function = DbObject::new(ObjectKind::Function, schema_path(), "f");
        assert!(matches!(
            s.script(function, Script::InsertTemplate),
            Err(BrowserError::Ddl(DdlError::Unsupported { .. }))
        ));
        assert_eq!(s.connector.connects(), 0);
    }

    #[test]
    fn connect_failure_is_reported() {
        let s = setup();
        let connector = s.connector.clone().failing_connect("no route");
        let err = s
            .script_with(&connector, view_obj(), Script::Create, CancelFlag::new())
            .unwrap_err();
        assert!(matches!(err, BrowserError::Connect(_)), "{err}");
        assert_eq!(connector.closed(), 0);
    }

    #[test]
    fn cancel_before_start_does_not_connect() {
        let s = setup();
        let cancel = CancelFlag::new();
        cancel.cancel();
        let err = s
            .script_with(&s.connector, view_obj(), Script::Create, cancel)
            .unwrap_err();
        assert!(matches!(err, BrowserError::Cancelled));
        assert_eq!(s.connector.connects(), 0);
    }

    #[test]
    fn cancel_while_connecting_closes_the_session() {
        let s = setup();
        let cancel = CancelFlag::new();
        let connector = s.connector.clone().cancelling_on_connect(cancel.clone());
        let err = s
            .script_with(&connector, view_obj(), Script::Create, cancel)
            .unwrap_err();
        assert!(matches!(err, BrowserError::Cancelled));
        assert_eq!((connector.connects(), connector.closed()), (1, 1));
        assert_eq!(connector.intro.definition_calls().len(), 0);
    }

    #[test]
    fn properties_of_a_table_include_columns() {
        let s = setup();
        let props =
            s.rt.block_on(load_properties(s.catalog.clone(), table_obj()))
                .unwrap();
        assert!(props.iter().any(|p| p.label == "Columns" && p.value == "2"));
        let schema = DbObject::new(ObjectKind::Schema, ObjectPath::root(), "app");
        let props =
            s.rt.block_on(load_properties(s.catalog.clone(), schema))
                .unwrap();
        assert_eq!(props.len(), 2);
        let gone = DbObject::new(ObjectKind::View, schema_path(), "gone");
        let err =
            s.rt.block_on(load_properties(s.catalog.clone(), gone))
                .unwrap_err();
        assert!(matches!(err, BrowserError::NotFound));
    }

    #[test]
    fn run_drop_executes_exactly_the_sql_and_closes() {
        let s = setup();
        let sql = "DROP TABLE \"public\".\"users\";\n".to_owned();
        s.rt.block_on(run_drop(
            Arc::new(s.connector.clone()),
            request(),
            sql.clone(),
        ))
        .unwrap();
        assert_eq!(s.connector.sql(), vec![sql]);
        assert_eq!((s.connector.connects(), s.connector.closed()), (1, 1));
    }

    #[test]
    fn run_drop_failure_closes_and_reports() {
        let s = setup();
        let connector =
            SessionConnector::new(FakeIntrospect::new()).failing_execute("table is referenced");
        let err =
            s.rt.block_on(run_drop(
                Arc::new(connector.clone()),
                request(),
                "DROP TABLE t;".into(),
            ))
            .unwrap_err();
        assert_eq!(err.to_string(), "query failed: table is referenced");
        assert_eq!(connector.closed(), 1);
        let connector = s.connector.clone().failing_connect("no route");
        let err =
            s.rt.block_on(run_drop(
                Arc::new(connector.clone()),
                request(),
                "DROP TABLE t;".into(),
            ))
            .unwrap_err();
        assert!(matches!(err, BrowserError::Connect(_)));
        assert_eq!(connector.sql().len(), 0);
    }

    fn wait<F: Future>(rt: &Runtime, fut: F) -> F::Output {
        rt.block_on(fut)
    }

    fn drain(model: &mut crate::grid::GridModel) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while matches!(model.state(), IngestState::Waiting | IngestState::Streaming) {
            assert!(Instant::now() < deadline, "grid did not finish");
            model.poll(usize::MAX);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn open_data_streams_rows_and_closes() {
        let s = setup();
        let connector = s.connector.clone().with_rows(3);
        let (mut grid, sink) = grid_channel();
        let sql = "SELECT *\nFROM \"public\".\"users\"\nLIMIT 1000;\n".to_owned();
        wait(
            &s.rt,
            open_data(Arc::new(connector.clone()), request(), sql.clone(), sink),
        )
        .unwrap();
        drain(&mut grid);
        assert!(matches!(grid.state(), IngestState::Done { .. }));
        assert_eq!(grid.row_count(), 3);
        assert_eq!(connector.sql(), vec![sql]);
        assert_eq!(connector.closed(), 1);
    }

    #[test]
    fn open_data_failures_reach_the_grid() {
        let s = setup();
        let (mut grid, sink) = grid_channel();
        let connector = s.connector.clone().failing_connect("no route");
        let err = wait(
            &s.rt,
            open_data(Arc::new(connector), request(), "SELECT 1".into(), sink),
        )
        .unwrap_err();
        assert!(matches!(err, BrowserError::Connect(_)));
        drain(&mut grid);
        assert!(
            matches!(grid.state(), IngestState::Failed(_)),
            "{:?}",
            grid.state()
        );

        let (mut grid, sink) = grid_channel();
        let connector = s.connector.clone().failing_execute("bad sql");
        let err = wait(
            &s.rt,
            open_data(
                Arc::new(connector.clone()),
                request(),
                "SELECT 1".into(),
                sink,
            ),
        )
        .unwrap_err();
        assert!(matches!(err, BrowserError::Driver(_)));
        drain(&mut grid);
        assert!(matches!(grid.state(), IngestState::Failed(_)));
        assert_eq!(connector.closed(), 1);
    }

    #[test]
    fn open_data_cancel_before_start_does_not_connect() {
        let s = setup();
        let (mut grid, sink) = grid_channel();
        grid.cancel();
        let err = wait(
            &s.rt,
            open_data(
                Arc::new(s.connector.clone()),
                request(),
                "SELECT 1".into(),
                sink,
            ),
        )
        .unwrap_err();
        assert!(matches!(err, BrowserError::Cancelled));
        assert_eq!(s.connector.connects(), 0);
    }

    fn wait_for_state(model: &mut crate::grid::GridModel, want: &IngestState) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while model.state() != want {
            assert!(Instant::now() < deadline, "stuck in {:?}", model.state());
            model.poll(usize::MAX);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn open_data_pauses_at_the_page_and_fetch_more_finishes() {
        let s = setup();
        let connector = s.connector.clone().with_rows(10);
        let (mut grid, sink) = grid_channel_with(GridLimits::rows(4));
        let job = s.rt.spawn(open_data(
            Arc::new(connector.clone()),
            request(),
            "SELECT 1".into(),
            sink,
        ));
        wait_for_state(&mut grid, &IngestState::Paused);
        assert_eq!(grid.row_count(), 4);
        assert_eq!(connector.closed(), 0);
        assert!(!job.is_finished());

        assert!(grid.fetch_more());
        wait_for_state(&mut grid, &IngestState::Paused);
        assert_eq!(grid.row_count(), 8);
        assert!(grid.fetch_more());
        wait_for_state(
            &mut grid,
            &IngestState::Done {
                rows_affected: None,
            },
        );
        assert_eq!(grid.row_count(), 10);
        s.rt.block_on(job).unwrap().unwrap();
        assert_eq!(connector.closed(), 1);
    }

    #[test]
    fn open_data_drop_while_paused_closes_the_session() {
        let s = setup();
        let connector = s.connector.clone().with_rows(10);
        let (mut grid, sink) = grid_channel_with(GridLimits::rows(4));
        let job = s.rt.spawn(open_data(
            Arc::new(connector.clone()),
            request(),
            "SELECT 1".into(),
            sink,
        ));
        wait_for_state(&mut grid, &IngestState::Paused);
        drop(grid);
        let err = s.rt.block_on(job).unwrap().unwrap_err();
        assert!(matches!(err, BrowserError::Cancelled), "{err:?}");
        assert_eq!(connector.closed(), 1);
    }
}
