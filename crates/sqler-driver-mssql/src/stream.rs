// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The two halves of a statement's event stream.
//!
//! The worker half ([`run_statement`]) runs inside the task that owns the connection: it sends
//! the statement, turns the first result set into `QueryEvent`s and pushes them through a bounded
//! channel. The consumer half ([`ChannelStream`]) is what the caller polls.

use std::borrow::Cow;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::Arc;

use futures_core::Stream as _;
use sqler_driver_api::sqler_core::{Query, Row};
use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};
use tiberius::{CommandItem, QueryItem, ResultMetadata};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::convert::{bind_named, column, declaration, decode_row};
use crate::session::{Client, Shared, query_error};

/// Rows per `QueryEvent::Rows`.
const BATCH: usize = 256;

/// One item on the channel between the worker and the consumer.
pub(crate) type Event = Result<QueryEvent, DriverError>;

/// How a statement ended, as far as the worker is concerned.
pub(crate) enum Outcome {
    /// The statement ended, or the consumer went away; the connection is still usable.
    Finished,
    /// A cancel arrived; the connection must be closed.
    Cancelled,
    /// The connection failed; it must be closed. The error is already sent to the consumer.
    Broken,
}

/// Awaits `fut` unless `cancel` fires first.
async fn or_cancel<F: Future>(cancel: &CancellationToken, fut: F) -> Option<F::Output> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        out = fut => Some(out),
    }
}

enum Sent {
    Yes,
    ConsumerGone,
    Cancelled,
}

async fn send(tx: &mpsc::Sender<Event>, cancel: &CancellationToken, event: Event) -> Sent {
    match or_cancel(cancel, tx.send(event)).await {
        None => Sent::Cancelled,
        Some(Ok(())) => Sent::Yes,
        Some(Err(_)) => Sent::ConsumerGone,
    }
}

/// Sends `event` and maps a lost consumer or a cancel to the outcome to stop with.
macro_rules! emit {
    ($tx:expr, $cancel:expr, $event:expr) => {
        match send($tx, $cancel, $event).await {
            Sent::Yes => {}
            Sent::ConsumerGone => return Outcome::Finished,
            Sent::Cancelled => return Outcome::Cancelled,
        }
    };
}

/// Runs `query` on `client` and streams the first result set to `tx`.
pub(crate) async fn run_statement(
    client: &mut Client,
    query: &Query,
    tx: &mpsc::Sender<Event>,
    cancel: &CancellationToken,
) -> Outcome {
    let counted = reports_count(&query.sql);
    let started = if counted {
        // DML runs as `sp_executesql` so the server's `DONE` counts reach us.
        let mut cmd = tiberius::Command::new("sp_executesql");
        cmd.bind_param("@stmt", Cow::<'static, str>::Owned(query.sql.clone()));
        if !query.params.is_empty() {
            cmd.bind_param(
                "@params",
                Cow::<'static, str>::Owned(declaration(&query.params)),
            );
            for (i, param) in query.params.iter().enumerate() {
                bind_named(&mut cmd, format!("@P{}", i + 1), param);
            }
        }
        or_cancel(cancel, cmd.exec(client))
            .await
            .map(|r| r.map(Results::Command))
    } else if query.params.is_empty() {
        // A plain batch keeps the session state (`USE`, `SET`) between statements.
        or_cancel(cancel, client.simple_query(query.sql.clone()))
            .await
            .map(|r| r.map(Results::Query))
    } else {
        let mut bound = tiberius::Query::new(query.sql.clone());
        for param in &query.params {
            crate::convert::bind(&mut bound, param);
        }
        or_cancel(cancel, bound.query(client))
            .await
            .map(|r| r.map(Results::Query))
    };
    let mut stream = match started {
        None => return Outcome::Cancelled,
        Some(Ok(stream)) => stream,
        Some(Err(e)) => return fail(e, tx, cancel).await,
    };

    let mut sets = 0_usize;
    let mut counts: Vec<u64> = Vec::new();
    let mut batch: Vec<Row> = Vec::new();
    loop {
        let item = match or_cancel(cancel, stream.next()).await {
            None => return Outcome::Cancelled,
            Some(None) => break,
            Some(Some(Err(e))) => return fail(e, tx, cancel).await,
            Some(Some(Ok(item))) => item,
        };
        match item {
            Item::Metadata(meta) => {
                sets += 1;
                if sets == 1 {
                    let columns = meta.columns().iter().map(column).collect();
                    emit!(tx, cancel, Ok(QueryEvent::Columns(columns)));
                }
            }
            // Later result sets are drained and ignored.
            Item::Row(row) if sets == 1 => match decode_row(&row) {
                Ok(row) => {
                    batch.push(row);
                    if batch.len() >= BATCH {
                        let rows = std::mem::take(&mut batch);
                        emit!(tx, cancel, Ok(QueryEvent::Rows(rows)));
                    }
                }
                Err(e) => {
                    let _ = send(tx, cancel, Err(e)).await;
                    return Outcome::Finished;
                }
            },
            Item::Row(_) | Item::Other => {}
            Item::Count(n) => counts.push(n),
        }
    }
    if sets == 0 {
        emit!(tx, cancel, Ok(QueryEvent::Columns(Vec::new())));
    }
    if !batch.is_empty() {
        emit!(tx, cancel, Ok(QueryEvent::Rows(batch)));
    }
    emit!(
        tx,
        cancel,
        Ok(QueryEvent::Done {
            rows_affected: rows_affected(counted, &counts)
        })
    );
    Outcome::Finished
}

/// One decoded item of either stream type.
enum Item {
    Metadata(ResultMetadata),
    Row(tiberius::Row),
    /// The count of one `DONE` or `DONEINPROC` token.
    Count(u64),
    /// A return status or an OUT value; not used.
    Other,
}

/// The stream of a started statement, whichever route started it.
enum Results<'a> {
    Query(tiberius::QueryStream<'a>),
    Command(tiberius::CommandStream<'a>),
}

impl Results<'_> {
    async fn next(&mut self) -> Option<Result<Item, tiberius::error::Error>> {
        match self {
            Self::Query(s) => poll_fn(|cx| Pin::new(&mut *s).poll_next(cx))
                .await
                .map(|r| {
                    r.map(|item| match item {
                        QueryItem::Metadata(m) => Item::Metadata(m),
                        QueryItem::Row(r) => Item::Row(r),
                    })
                }),
            Self::Command(s) => poll_fn(|cx| Pin::new(&mut *s).poll_next(cx))
                .await
                .map(|r| {
                    r.map(|item| match item {
                        CommandItem::Metadata(m) => Item::Metadata(m),
                        CommandItem::Row(r) => Item::Row(r),
                        CommandItem::RowsAffected(n) => Item::Count(n),
                        CommandItem::ReturnStatus(_) | CommandItem::ReturnValue(_) => Item::Other,
                    })
                }),
        }
    }
}

/// Words that start a statement which can change session state, end a transaction or otherwise
/// must not run inside `sp_executesql` (its session state, `SET`, `USE` and `#temp` tables, ends
/// with the call). Seen anywhere outside a literal, comment or quoted name, one of them sends the
/// whole text down the plain-batch route.
const BATCH_ONLY: &[&str] = &[
    "USE",
    "DECLARE",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "EXEC",
    "EXECUTE",
    "BEGIN",
    "IF",
    "WHILE",
    "GO",
    "GOTO",
    "RETURN",
    "COMMIT",
    "ROLLBACK",
    "SAVE",
    "PRINT",
    "RAISERROR",
    "THROW",
    "WAITFOR",
    "OPEN",
    "CLOSE",
    "FETCH",
    "DEALLOCATE",
    "GRANT",
    "REVOKE",
    "DENY",
    "DBCC",
    "KILL",
    "BACKUP",
    "RESTORE",
    "RECONFIGURE",
    "SETUSER",
    "BULK",
];

/// The words of `sql` in upper case, with `;` as its own `";"` entry. String literals, quoted
/// names, comments and every other punctuation character are dropped.
fn words(sql: &str) -> Vec<String> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 1_usize;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if matches!(c, b'\'' | b'"' | b'[') {
            // A doubled closing character is an escape (`''`, `""`, `]]`).
            let close = if c == b'[' { b']' } else { c };
            i += 1;
            while i < b.len() {
                if b[i] == close {
                    if b.get(i + 1) == Some(&close) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
        } else if c == b';' {
            out.push(";".to_owned());
            i += 1;
        } else if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'@' | b'#' | b'$') {
            let start = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'@' | b'#' | b'$'))
            {
                i += 1;
            }
            out.push(sql[start..i].to_ascii_uppercase());
        } else {
            i += 1;
        }
    }
    out
}

/// Whether `sql` is confidently one DML statement, so running it inside `sp_executesql` loses no
/// session state. It must start with `INSERT`, `UPDATE`, `DELETE`, `MERGE` or `WITH` (not
/// `UPDATE STATISTICS`), and none of these may appear: a `;` followed by more text, a word of
/// [`BATCH_ONLY`], more `SET`s than `UPDATE`s (a `SET` statement after an `UPDATE ... SET`), or an
/// `INTO` that is not `INSERT INTO` / `MERGE INTO` (`SELECT ... INTO #tmp`). Anything unsure takes
/// the batch route. `UPDATE ... SET`, `INSERT ... SELECT` and `WITH ... INSERT/UPDATE/DELETE/MERGE`
/// are single statements.
fn reports_count(sql: &str) -> bool {
    let words = words(sql);
    let mut iter = words.iter().map(String::as_str).skip_while(|w| *w == ";");
    let first = iter.next();
    if !matches!(
        first,
        Some("INSERT" | "UPDATE" | "DELETE" | "MERGE" | "WITH")
    ) {
        return false;
    }
    if first == Some("UPDATE") && iter.next() == Some("STATISTICS") {
        return false;
    }
    let list: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .skip_while(|w| *w == ";")
        .collect();
    // Only `;` may follow the first `;`.
    if let Some(semi) = list.iter().position(|w| *w == ";")
        && list[semi..].iter().any(|w| *w != ";")
    {
        return false;
    }
    let count = |name: &str| list.iter().filter(|w| **w == name).count();
    if count("SET") > count("UPDATE") {
        return false;
    }
    for (i, w) in list.iter().enumerate() {
        if BATCH_ONLY.contains(w) {
            return false;
        }
        if *w == "INTO" && !(i > 0 && matches!(list[i - 1], "INSERT" | "MERGE")) {
            return false;
        }
    }
    true
}

/// The rows-affected figure of a statement: the single count of a counted statement. No count
/// (`SET NOCOUNT ON`) or several (a trigger that counts its own statements) is unknown.
fn rows_affected(counted: bool, counts: &[u64]) -> Option<u64> {
    match (counted, counts) {
        (true, [n]) => Some(*n),
        _ => None,
    }
}

/// Sends a tiberius error. A server error leaves the connection usable; anything else breaks it.
async fn fail(
    e: tiberius::error::Error,
    tx: &mpsc::Sender<Event>,
    cancel: &CancellationToken,
) -> Outcome {
    let server = matches!(e, tiberius::error::Error::Server(_));
    let _ = send(tx, cancel, Err(query_error(&e))).await;
    if server {
        Outcome::Finished
    } else {
        Outcome::Broken
    }
}

/// The event stream of one statement, as the caller sees it.
pub(crate) struct ChannelStream {
    rx: mpsc::Receiver<Event>,
    shared: Arc<Shared>,
    ended: bool,
}

impl ChannelStream {
    pub(crate) fn new(rx: mpsc::Receiver<Event>, shared: Arc<Shared>) -> Self {
        Self {
            rx,
            shared,
            ended: false,
        }
    }

    async fn step(&mut self) -> Option<Event> {
        if self.ended {
            return None;
        }
        let event = self.rx.recv().await;
        match event {
            Some(Ok(QueryEvent::Done { .. }) | Err(_)) => self.end(),
            Some(Ok(_)) => {}
            None => {
                self.end();
                return Some(Err(if self.shared.was_cancelled() {
                    DriverError::Cancelled
                } else {
                    DriverError::Connection("the SQL Server connection was lost".to_owned())
                }));
            }
        }
        event
    }

    fn end(&mut self) {
        self.ended = true;
        self.shared.finish();
    }
}

impl Drop for ChannelStream {
    fn drop(&mut self) {
        self.shared.finish();
    }
}

impl QueryStream for ChannelStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Event>> {
        Box::pin(self.step())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dml_keywords_report_a_count() {
        for sql in [
            "insert into t values (1)",
            "UPDATE t set a = 1",
            "  \n\tDelete from t",
            "merge t using s on 1=1 when matched then delete;",
            "with c as (select 1 a) update c set a = 2",
            ";WITH c as (select 1 a) select * from c",
            "-- note\nupdate t set a = 1",
            "/* a /* nested */ still */ insert into t values (1)",
            "/* x */ -- y\n ; update t set a = 1",
            "update",
            "update t set a = 1;",
            "update t set a = 1;;  -- done",
            "update t set a = ';set x' where b = 'use'",
            "update t set [set] = 1, \"use\" = 2",
            "insert into t select a from s where a > 0",
            "insert into #tmp values (1)",
            "with c as (select 1 a) update c set a = 2",
            "with c as (select 1 a) insert into t select a from c",
            "merge t using s on 1=1 when matched then update set a = 1 when not matched then insert values (1)",
            "delete from t output deleted.a",
            "update t set a = 1 update u set b = 2",
        ] {
            assert!(reports_count(sql), "{sql}");
        }
    }

    #[test]
    fn text_that_may_hold_session_state_keeps_the_batch_route() {
        for sql in [
            "insert into t values (1)\nset nocount on",
            "update t set a = 1\nset nocount on",
            "update t set a = 1; set nocount on",
            "update t set a = 1;\nuse otherdb",
            "insert into t values (1)\nuse otherdb",
            "update t set a = 1\ndeclare @x int",
            "update t set a = 1\ncreate table #t (a int)",
            "update t set a = 1\ndrop table #t",
            "update t set a = 1\nexec sp_who",
            "update t set a = 1\nbegin tran",
            "update t set a = 1\nif 1 = 1 select 1",
            "update t set a = 1\nwhile 0 = 1 select 1",
            "update t set a = 1\nselect * into #tmp from t",
            "with c as (select 1 a) select * into #tmp from c",
            "update t set a = 1 output inserted.a into @v",
            "delete from t; select 1",
            "update t set a = 1\ncommit",
        ] {
            assert!(!reports_count(sql), "{sql}");
        }
    }

    #[test]
    fn update_statistics_is_not_dml() {
        assert!(!reports_count("update statistics t"));
        assert!(!reports_count(
            "  -- x\nUPDATE   STATISTICS dbo.t with fullscan"
        ));
        assert!(reports_count("update statisticsx set a = 1"));
    }

    #[test]
    fn other_statements_do_not() {
        for sql in [
            "",
            "   ",
            "UPDATEX t set a = 1",
            "select 1",
            "set nocount on",
            "begin transaction",
            "exec sp_who",
            "create table t (a int)",
            "-- update\nselect 1",
            "/* update */ select 1",
            "/* unterminated update",
        ] {
            assert!(!reports_count(sql), "{sql}");
        }
    }

    #[test]
    fn rows_affected_needs_exactly_one_count() {
        assert_eq!(rows_affected(true, &[]), None);
        assert_eq!(rows_affected(true, &[3]), Some(3));
        assert_eq!(rows_affected(true, &[0]), Some(0));
        assert_eq!(rows_affected(true, &[1, 1]), None);
        assert_eq!(rows_affected(false, &[5]), None);
    }
}
