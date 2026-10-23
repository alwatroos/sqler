// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `export` command: stream the rows of one read statement into an export writer.
//!
//! The SQL is classified before any connection is made. Rows go from the driver stream into the
//! writer one batch at a time, so memory does not depend on the row count. A file is written to a
//! hidden sibling and renamed over the target only after the writer has finished, so a failed or
//! cancelled export leaves no partial file and never touches an existing target.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use anyhow::{Context, anyhow};
use sqler_driver_api::sqler_core::Query;
use sqler_driver_api::{DriverError, QueryEvent, QueryStream};
use sqler_export::{
    CSV, CancelToken, ExportControl, ExportError, ExportOptions, Exporter, Format, JSON, MARKDOWN,
    Registry, SQL, Sink, Summary,
};
use sqler_sql::{Dialect, StatementClass, classify_each};
use tokio::sync::Notify;

use crate::args::{ExportArgs, Output, USAGE};
use crate::{connection_config, registry};

/// The formats that may go to stdout. Any other id is treated as binary (fail safe).
const TEXT_FORMATS: [&str; 4] = [CSV, JSON, SQL, MARKDOWN];

/// Why an export did not succeed, with the exit code of each kind.
#[derive(Debug)]
pub(crate) enum Failure {
    /// A bad command line (exit 2).
    Usage(String),
    /// The write gate refused the statement (exit 3).
    Refused(String),
    /// Ctrl+C (exit 130). A second signal while the export winds down exits 130 at once.
    Cancelled,
    /// SIGTERM (exit 143).
    Terminated,
    /// Anything else (exit 1).
    Runtime(anyhow::Error),
}

impl Failure {
    /// The process exit code.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::Runtime(_) => 1,
            Self::Usage(_) => 2,
            Self::Refused(_) => 3,
            Self::Cancelled => 130,
            Self::Terminated => 143,
        }
    }

    /// The text to print after `error: `. A usage failure gets the usage text appended.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Usage(msg) => format!("{msg}\n\n{USAGE}"),
            other => other.to_string(),
        }
    }
}

// The target never appears here: nothing in a `Failure` is built from it.
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(msg) | Self::Refused(msg) => f.write_str(msg),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Terminated => f.write_str("terminated"),
            Self::Runtime(err) => write!(f, "{err:#}"),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(err: anyhow::Error) -> Self {
        Self::Runtime(err)
    }
}

fn driver_failure(err: DriverError) -> Failure {
    match err {
        DriverError::Cancelled => Failure::Cancelled,
        other => Failure::Runtime(anyhow!("{}", crate::driver_message(&other))),
    }
}

fn export_failure(err: ExportError) -> Failure {
    match err {
        ExportError::Cancelled => Failure::Cancelled,
        // The variant's own text repeats the cause that the error chain already prints.
        ExportError::Io(err) => Failure::Runtime(anyhow::Error::new(err).context("write failed")),
        // Their texts can carry option and identifier values from the command line.
        ExportError::InvalidOptions(_) => Failure::Runtime(anyhow!(
            "the export options cannot be used with this format"
        )),
        ExportError::Sql(_) => Failure::Runtime(anyhow!(
            "the --table or --schema name cannot be used as an SQL identifier"
        )),
        _ => Failure::Runtime(anyhow!("export failed")),
    }
}

/// The write gate. Returns the warning to print when a non-read statement is allowed.
fn gate(args: &ExportArgs) -> Result<Option<String>, Failure> {
    let dialect = Dialect::from_driver_id(&args.driver);
    let segments = classify_each(dialect.unwrap_or(Dialect::Postgres), &args.sql);
    if segments.len() != 1 {
        return Err(Failure::Usage(
            "export takes exactly one statement".to_owned(),
        ));
    }
    // Without a dialect nothing is known about the syntax: never a read.
    let class = if dialect.is_some() {
        segments[0].1
    } else {
        StatementClass::Unknown
    };
    if class.is_read_only() {
        return Ok(None);
    }
    if !args.allow_write {
        return Err(Failure::Refused(format!(
            "refusing to export this statement (class: {}); pass --allow-write to run it",
            class.as_str()
        )));
    }
    Ok(Some(format!(
        "warning: running a statement of class {} (--allow-write)",
        class.as_str()
    )))
}

/// Looks up the format and builds its options.
fn resolve_format<'r>(
    registry: &'r Registry,
    args: &ExportArgs,
) -> Result<(&'r dyn Format, ExportOptions), Failure> {
    // Only literals and registered ids are printed, never the argument itself.
    let id = args.format.as_str();
    let Some(format) = registry.get(id) else {
        let compiled_out = match id {
            "parquet" => Some("parquet"),
            "xlsx" => Some("xlsx"),
            _ => None,
        };
        if let Some(name) = compiled_out {
            return Err(Failure::Usage(format!(
                "format `{name}` is not available in this build of sqler-cli (built without the \
                 `export-{name}` feature)"
            )));
        }
        let known: Vec<&str> = registry.formats().map(Format::id).collect();
        return Err(Failure::Usage(format!(
            "unknown format; available: {}",
            known.join(", ")
        )));
    };
    if args.output == Output::Stdout && !TEXT_FORMATS.contains(&format.id()) {
        return Err(Failure::Usage(format!(
            "{} is a binary format; --output - is not allowed",
            format.id()
        )));
    }
    let mut options = ExportOptions::default();
    if format.id() == SQL {
        let table = args
            .table
            .clone()
            .ok_or_else(|| Failure::Usage("the sql format needs --table".to_owned()))?;
        let dialect = Dialect::from_driver_id(&args.driver).ok_or_else(|| {
            Failure::Usage("the sql format does not support this driver".to_owned())
        })?;
        options.sql.table = table;
        options.sql.schema.clone_from(&args.schema);
        options.sql.dialect = dialect;
    }
    Ok((format, options))
}

/// A hidden sibling of the target. It becomes the target on [`commit`](Self::commit) and is
/// deleted on drop otherwise.
struct PartialFile {
    path: PathBuf,
    target: PathBuf,
    keep: Option<File>,
    committed: bool,
}

impl PartialFile {
    /// Creates the temporary file; returns it with the handle to write to.
    fn create(target: &Path) -> anyhow::Result<(Self, File)> {
        if target.is_dir() {
            return Err(anyhow!("--output is a directory"));
        }
        let name = target
            .file_name()
            .ok_or_else(|| anyhow!("--output has no file name"))?;
        let dir = match target.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut partial_name = std::ffi::OsString::from(".");
        partial_name.push(name);
        partial_name.push(format!(".{}.partial", std::process::id()));
        let path = dir.join(partial_name);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .context("cannot create the output file")?;
        let keep = file
            .try_clone()
            .context("cannot duplicate the output handle")?;
        Ok((
            Self {
                path,
                target: target.to_owned(),
                keep: Some(keep),
                committed: false,
            },
            file,
        ))
    }

    /// Syncs the file and renames it over the target.
    fn commit(mut self) -> anyhow::Result<()> {
        if let Some(file) = self.keep.take() {
            file.sync_all().context("cannot sync the output file")?;
        }
        fs::rename(&self.path, &self.target).context("cannot write the output file")?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.committed {
            self.keep = None;
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// What a pumped stream produced.
#[derive(Debug)]
pub(crate) struct Pumped {
    /// Rows and bytes written.
    pub(crate) summary: Summary,
    /// Rows affected, if the backend reports it.
    pub(crate) rows_affected: Option<u64>,
    /// The number of result columns (0 for a statement without a result set).
    pub(crate) columns: usize,
}

fn protocol(msg: &str) -> Failure {
    Failure::Runtime(anyhow!("driver protocol error: {msg}"))
}

/// Moves the events of `stream` into `exporter`, one batch at a time. A batch is dropped as soon as
/// it is written.
pub(crate) async fn pump(
    stream: &mut (dyn QueryStream + '_),
    mut exporter: Exporter<'_>,
) -> Result<Pumped, Failure> {
    let mut columns: Option<usize> = None;
    loop {
        let Some(event) = stream.next_event().await else {
            return Err(protocol("stream ended before Done"));
        };
        match event.map_err(driver_failure)? {
            QueryEvent::Columns(cols) => {
                if columns.is_some() {
                    return Err(protocol("second Columns event"));
                }
                // A statement without a result set has nothing to write.
                if !cols.is_empty() {
                    exporter.begin(&cols).map_err(export_failure)?;
                }
                columns = Some(cols.len());
            }
            QueryEvent::Rows(batch) => {
                match columns {
                    None => return Err(protocol("Rows before Columns")),
                    Some(0) if batch.is_empty() => continue,
                    Some(0) => return Err(protocol("Rows for a statement without columns")),
                    Some(_) => {}
                }
                exporter.write_batch(&batch).map_err(export_failure)?;
            }
            QueryEvent::Done { rows_affected } => {
                let Some(columns) = columns else {
                    return Err(protocol("Done before Columns"));
                };
                let summary = if columns == 0 {
                    Summary::default()
                } else {
                    exporter.finish().map_err(export_failure)?
                };
                return Ok(Pumped {
                    summary,
                    rows_affected,
                    columns,
                });
            }
            _ => {}
        }
    }
}

/// Runs `export` on its own current-thread runtime.
pub(crate) fn run(args: &ExportArgs) -> Result<(), Failure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    runtime.block_on(run_export(args))
}

/// Listens for SIGINT and SIGTERM (Ctrl+C elsewhere). The handlers are installed when this is
/// created, so a signal after that point is never handled by the default action, which would kill
/// the process and leave the partial file behind.
struct Interrupts {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl Interrupts {
    #[cfg(unix)]
    fn install() -> Result<Self, Failure> {
        use tokio::signal::unix::{SignalKind, signal};
        let listen = |kind| {
            signal(kind)
                .context("cannot listen for signals")
                .map_err(Failure::from)
        };
        Ok(Self {
            interrupt: listen(SignalKind::interrupt())?,
            terminate: listen(SignalKind::terminate())?,
        })
    }

    #[cfg(not(unix))]
    #[allow(clippy::unnecessary_wraps)]
    fn install() -> Result<Self, Failure> {
        Ok(Self {})
    }

    /// Resolves with the failure of the signal that arrived.
    #[cfg(unix)]
    async fn wait(&mut self) -> Failure {
        tokio::select! {
            _ = self.interrupt.recv() => Failure::Cancelled,
            _ = self.terminate.recv() => Failure::Terminated,
        }
    }

    #[cfg(not(unix))]
    async fn wait(&mut self) -> Failure {
        match tokio::signal::ctrl_c().await {
            Ok(()) => Failure::Cancelled,
            Err(_) => std::future::pending().await,
        }
    }
}

async fn run_export(args: &ExportArgs) -> Result<(), Failure> {
    let drivers = registry()?;
    // Before the gate: an unknown driver is a runtime error, not a refusal to write.
    let id = crate::driver_id(&args.driver)?;
    // Built-in drivers and the available drivers of plugins; one that is unavailable ends as unknown here.
    if !drivers.available_ids().contains(&id) {
        return Err(Failure::Runtime(anyhow!(crate::UNKNOWN_DRIVER)));
    }
    let warning = gate(args)?;
    let formats = Registry::with_builtin();
    let (format, options) = resolve_format(&formats, args)?;
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }

    let mut interrupts = Interrupts::install()?;
    let config = connection_config(&args.driver, &args.target)?;
    let connected = tokio::select! {
        session = drivers.connect(&config) => session,
        failure = interrupts.wait() => return Err(failure),
    };
    let mut session = connected.map_err(driver_failure)?;

    let token = CancelToken::new();
    let signalled = Arc::new(AtomicU8::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let cancel_sent = Arc::new(Notify::new());
    let handle = session.cancel_handle();
    let watcher = {
        let token = token.clone();
        let signalled = Arc::clone(&signalled);
        let finished = Arc::clone(&finished);
        let cancel_sent = Arc::clone(&cancel_sent);
        tokio::spawn(async move {
            let first = interrupts.wait().await;
            // A signal once the export is over (while the session closes) is not a cancel: leave
            // at once.
            if finished.load(Ordering::SeqCst) {
                std::process::exit(i32::from(first.exit_code()));
            }
            signalled.store(first.exit_code(), Ordering::SeqCst);
            token.cancel();
            // The cancel request is sent from this task and awaited before the task ends, so it
            // is never dropped half-way. A second signal gives up on it.
            tokio::select! {
                _ = handle.cancel() => {}
                second = interrupts.wait() => std::process::exit(i32::from(second.exit_code())),
            }
            cancel_sent.notify_one();
            // Still listening: a second signal forces the exit, even while the session closes.
            let second = interrupts.wait().await;
            std::process::exit(i32::from(second.exit_code()));
        })
    };
    // Let the watcher run once: a signal that arrived during connect is seen before any file is
    // created, and elsewhere than on Unix this is where the Ctrl+C handler gets installed.
    tokio::task::yield_now().await;

    let result = export_stream(&mut *session, args, format, &options, &token).await;
    finished.store(true, Ordering::SeqCst);
    if signalled.load(Ordering::SeqCst) != 0 {
        // The server-side cancel goes out before the session is closed.
        cancel_sent.notified().await;
    }
    let closed = session.close().await;
    watcher.abort();
    let pumped = match result {
        Ok(pumped) => pumped,
        Err(_) if token.is_cancelled() => {
            return Err(match signalled.load(Ordering::SeqCst) {
                143 => Failure::Terminated,
                _ => Failure::Cancelled,
            });
        }
        Err(err) => return Err(err),
    };
    closed.map_err(driver_failure)?;

    let Pumped {
        summary,
        rows_affected,
        columns,
    } = pumped;
    if columns == 0 {
        match rows_affected {
            Some(n) => eprintln!("({n} rows affected)"),
            None => eprintln!("(done)"),
        }
    } else {
        let place = match &args.output {
            Output::Stdout => String::new(),
            Output::File(path) => format!(" to {}", path.display()),
        };
        eprintln!("({} rows, {} bytes){place}", summary.rows, summary.bytes);
    }
    Ok(())
}

async fn export_stream(
    session: &mut dyn sqler_driver_api::Session,
    args: &ExportArgs,
    format: &dyn Format,
    options: &ExportOptions,
    token: &CancelToken,
) -> Result<Pumped, Failure> {
    // The `SQLite` driver resets its cancel flag in `execute`: a signal before it must stop here,
    // and before a file exists.
    if token.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    let (partial, sink): (Option<PartialFile>, Sink<'static>) = match &args.output {
        Output::Stdout => (None, Box::new(io::stdout())),
        Output::File(path) => {
            let (partial, file) = PartialFile::create(path)?;
            (Some(partial), Box::new(file))
        }
    };
    let query = Query::new(&args.sql);
    let pumped = {
        let mut stream = session.execute(&query).await.map_err(driver_failure)?;
        let control = ExportControl {
            cancel: token.clone(),
            progress: None,
        };
        let exporter = Exporter::new(format, options, sink, control).map_err(export_failure)?;
        pump(&mut *stream, exporter).await?
    };
    // No result set, no file: the partial file is dropped (deleted) instead of committed.
    if let Some(partial) = partial.filter(|_| pumped.columns > 0) {
        partial.commit()?;
    }
    io::stdout()
        .flush()
        .map_err(|e| Failure::Runtime(e.into()))?;
    Ok(pumped)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use sqler_driver_api::BoxFuture;
    use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};
    use sqler_export::Progress;

    use super::*;

    fn args(driver: &str, sql: &str) -> ExportArgs {
        ExportArgs {
            driver: driver.to_owned(),
            target: "secret-target".to_owned(),
            sql: sql.to_owned(),
            format: "csv".to_owned(),
            output: Output::File(PathBuf::from("out.csv")),
            table: None,
            schema: None,
            allow_write: false,
        }
    }

    fn refused(sql: &str) -> String {
        match gate(&args("sqlite", sql)) {
            Err(Failure::Refused(msg)) => msg,
            other => panic!("expected a refusal for {sql:?}, got {other:?}"),
        }
    }

    #[test]
    fn gate_passes_reads() {
        assert!(gate(&args("sqlite", "select 1")).unwrap().is_none());
        assert!(gate(&args("sqlite", "select 1;")).unwrap().is_none());
        let cte = "with recursive c as (select 1 as x union all select x + 1 from c \
                   where x < 1000000) select x from c";
        assert!(gate(&args("sqlite", cte)).unwrap().is_none());
    }

    #[test]
    fn gate_refuses_everything_else() {
        assert!(refused("insert into t values (1)").contains("class: write"));
        assert!(refused("create table t (a int)").contains("class: ddl"));
        assert!(refused("begin").contains("class: other"));
        assert!(refused("zzz qqq").contains("class: unknown"));
    }

    #[test]
    fn gate_wants_exactly_one_statement() {
        for sql in ["select 1; select 2", "", "  "] {
            assert!(
                matches!(gate(&args("sqlite", sql)), Err(Failure::Usage(_))),
                "{sql:?}"
            );
        }
    }

    #[test]
    fn gate_allow_write_warns_without_the_sql() {
        let mut a = args("sqlite", "insert into t values (1)");
        a.allow_write = true;
        let warning = gate(&a).unwrap().unwrap();
        assert_eq!(
            warning,
            "warning: running a statement of class write (--allow-write)"
        );
        let mut a = args("sqlite", "create user u identified by 's3cret'");
        a.allow_write = true;
        let warning = gate(&a).unwrap().unwrap();
        assert!(!warning.contains("s3cret") && !warning.contains("identified"));
    }

    #[test]
    fn gate_unknown_driver_is_never_a_read() {
        assert!(matches!(
            gate(&args("nope", "select 1")),
            Err(Failure::Refused(_))
        ));
    }

    fn resolve(a: &ExportArgs) -> Result<ExportOptions, Failure> {
        let r = Registry::with_builtin();
        resolve_format(&r, a).map(|(_, o)| o)
    }

    #[test]
    fn format_unknown_lists_the_available_ones() {
        let mut a = args("sqlite", "select 1");
        a.format = "nope".to_owned();
        let Err(Failure::Usage(msg)) = resolve(&a) else {
            panic!("expected a usage error");
        };
        assert!(msg.contains("csv, json, sql, markdown"), "{msg}");
    }

    #[test]
    fn format_compiled_out_message() {
        let registry = Registry::with_builtin();
        for id in ["parquet", "xlsx"] {
            if registry.get(id).is_some() {
                continue;
            }
            let mut a = args("sqlite", "select 1");
            a.format = id.to_owned();
            let Err(Failure::Usage(msg)) = resolve(&a) else {
                panic!("expected a usage error");
            };
            assert!(msg.contains("not available in this build"), "{msg}");
            assert!(msg.contains(&format!("export-{id}")), "{msg}");
        }
    }

    #[test]
    fn format_binary_to_stdout_is_refused() {
        let registry = Registry::with_builtin();
        for id in ["parquet", "xlsx"] {
            if registry.get(id).is_none() {
                continue;
            }
            let mut a = args("sqlite", "select 1");
            a.format = id.to_owned();
            a.output = Output::Stdout;
            let Err(Failure::Usage(msg)) = resolve(&a) else {
                panic!("expected a usage error");
            };
            assert!(msg.contains("binary format"), "{msg}");
        }
    }

    #[test]
    fn format_sql_needs_table_and_dialect() {
        let mut a = args("sqlite", "select 1");
        a.format = "sql".to_owned();
        assert!(matches!(resolve(&a), Err(Failure::Usage(_))));
        a.table = Some("t".to_owned());
        a.schema = Some("s".to_owned());
        let o = resolve(&a).unwrap();
        assert_eq!(o.sql.table, "t");
        assert_eq!(o.sql.schema.as_deref(), Some("s"));
        assert_eq!(o.sql.dialect, Dialect::Sqlite);
        a.driver = "nope".to_owned();
        assert!(matches!(resolve(&a), Err(Failure::Usage(_))));
    }

    #[test]
    fn text_formats_go_to_stdout() {
        for id in TEXT_FORMATS {
            let mut a = args("sqlite", "select 1");
            a.format = id.to_owned();
            a.output = Output::Stdout;
            a.table = Some("t".to_owned());
            assert!(resolve(&a).is_ok(), "{id}");
        }
    }

    #[test]
    fn exit_codes() {
        assert_eq!(Failure::Runtime(anyhow!("x")).exit_code(), 1);
        assert_eq!(Failure::Usage(String::new()).exit_code(), 2);
        assert_eq!(Failure::Refused(String::new()).exit_code(), 3);
        assert_eq!(Failure::Cancelled.exit_code(), 130);
        assert_eq!(Failure::Cancelled.to_string(), "cancelled");
        assert!(Failure::Usage("x".to_owned()).message().contains("Usage:"));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sqler-cli-unit-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn partial_file_is_removed_on_drop() {
        let dir = scratch("drop");
        let target = dir.join("out.csv");
        let (partial, mut file) = PartialFile::create(&target).unwrap();
        file.write_all(b"abc").unwrap();
        assert_eq!(entries(&dir).len(), 1);
        drop(file);
        drop(partial);
        assert_eq!(entries(&dir), Vec::<String>::new());
    }

    #[test]
    fn partial_file_commit_replaces_the_target() {
        let dir = scratch("commit");
        let target = dir.join("out.csv");
        fs::write(&target, "old").unwrap();
        let (partial, mut file) = PartialFile::create(&target).unwrap();
        file.write_all(b"new").unwrap();
        drop(file);
        partial.commit().unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(entries(&dir), ["out.csv"]);
    }

    #[test]
    fn existing_target_survives_a_drop() {
        let dir = scratch("survive");
        let target = dir.join("out.csv");
        fs::write(&target, "old").unwrap();
        let (partial, file) = PartialFile::create(&target).unwrap();
        drop(file);
        drop(partial);
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(entries(&dir), ["out.csv"]);
    }

    #[test]
    fn partial_file_rejects_a_directory() {
        let dir = scratch("dir");
        assert!(PartialFile::create(&dir).is_err());
    }

    /// 1,000 batches of 1,000 rows. Each `next_event` checks how many produced rows the writer
    /// has not taken yet.
    struct FakeStream {
        step: u32,
        produced: Arc<AtomicU64>,
        written: Arc<AtomicU64>,
    }

    impl QueryStream for FakeStream {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            let step = self.step;
            self.step += 1;
            let event = match step {
                0 => Some(Ok(QueryEvent::Columns(vec![Column::new(
                    "x",
                    DataType::Integer,
                )]))),
                1..=1000 => {
                    let rows: Vec<Row> = (0..1000).map(|i| Row(vec![Value::Int(i)])).collect();
                    let produced = self.produced.fetch_add(1000, Ordering::SeqCst) + 1000;
                    let written = self.written.load(Ordering::SeqCst);
                    assert!(produced - written <= 1000, "more than one batch alive");
                    Some(Ok(QueryEvent::Rows(rows)))
                }
                1001 => Some(Ok(QueryEvent::Done {
                    rows_affected: None,
                })),
                _ => None,
            };
            Box::pin(async move { event })
        }
    }

    struct CountingSink(Arc<AtomicU64>);

    impl Write for CountingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.fetch_add(buf.len() as u64, Ordering::SeqCst);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn pump_holds_at_most_one_batch() {
        let produced = Arc::new(AtomicU64::new(0));
        let written = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));
        let mut stream = FakeStream {
            step: 0,
            produced: Arc::clone(&produced),
            written: Arc::clone(&written),
        };
        let seen = Arc::clone(&written);
        let control = ExportControl {
            cancel: CancelToken::new(),
            progress: Some(Box::new(move |p: Progress| {
                seen.store(p.rows, Ordering::SeqCst);
            })),
        };
        let exporter = Exporter::new(
            Registry::with_builtin().get(CSV).unwrap(),
            &ExportOptions::default(),
            Box::new(CountingSink(Arc::clone(&bytes))),
            control,
        )
        .unwrap();
        let pumped = block_on(pump(&mut stream, exporter)).unwrap();
        assert_eq!(pumped.summary.rows, 1_000_000);
        assert_eq!(produced.load(Ordering::SeqCst), 1_000_000);
        assert_eq!(written.load(Ordering::SeqCst), 1_000_000);
        assert_eq!(pumped.summary.bytes, bytes.load(Ordering::SeqCst));
        assert_eq!(pumped.columns, 1);
    }

    struct ScriptedStream(Vec<Option<Result<QueryEvent, DriverError>>>);

    impl QueryStream for ScriptedStream {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            let event = if self.0.is_empty() {
                None
            } else {
                self.0.remove(0)
            };
            Box::pin(async move { event })
        }
    }

    fn pump_script(
        events: Vec<Option<Result<QueryEvent, DriverError>>>,
    ) -> Result<Pumped, Failure> {
        let exporter = Exporter::new(
            Registry::with_builtin().get(CSV).unwrap(),
            &ExportOptions::default(),
            Box::new(io::sink()),
            ExportControl::default(),
        )
        .unwrap();
        block_on(pump(&mut ScriptedStream(events), exporter))
    }

    #[test]
    fn pump_enforces_the_protocol() {
        let rows = || Some(Ok(QueryEvent::Rows(vec![])));
        let done = || {
            Some(Ok(QueryEvent::Done {
                rows_affected: None,
            }))
        };
        assert!(matches!(
            pump_script(vec![rows()]),
            Err(Failure::Runtime(_))
        ));
        assert!(matches!(
            pump_script(vec![done()]),
            Err(Failure::Runtime(_))
        ));
        assert!(matches!(pump_script(vec![]), Err(Failure::Runtime(_))));
        let cols = || Some(Ok(QueryEvent::Columns(vec![])));
        assert!(matches!(
            pump_script(vec![cols(), cols()]),
            Err(Failure::Runtime(_))
        ));
        assert!(matches!(
            pump_script(vec![cols(), Some(Err(DriverError::Cancelled))]),
            Err(Failure::Cancelled)
        ));
        let ok = pump_script(vec![
            cols(),
            Some(Ok(QueryEvent::Done {
                rows_affected: Some(3),
            })),
        ])
        .unwrap();
        assert_eq!(ok.rows_affected, Some(3));
        assert_eq!(ok.columns, 0);
    }
}
