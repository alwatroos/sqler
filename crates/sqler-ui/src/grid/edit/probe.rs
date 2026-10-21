// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The schema the server resolves a query's table name to (ADR-0030, rule 4).
//!
//! `check_editable` trusts [`EditRequest::resolved_schema`](super::EditRequest): for an
//! unqualified `FROM t` only the server knows whether `t` is the loaded table or one that shadows
//! it (a `PostgreSQL` temporary table, a `SQLite` `temp` table). A [`SchemaProbe`] is that lookup:
//! one fixed, read-only `SELECT` per dialect, run on the session that ran the query, right after
//! it. The table name is a bound parameter and is never spliced into the text. Any failure gives
//! `None`, and `None` makes the result read-only (fail-safe).

use sqler_driver_api::sqler_core::{Query, Value};
use sqler_driver_api::{BoxFuture, Session, collect};
use sqler_sql::{Dialect, quote_identifier, single_table_source};

/// `PostgreSQL`: the schema of the relation `to_regclass` finds, which searches `pg_temp` first.
const PG_SQL: &str = "SELECT n.nspname FROM pg_catalog.pg_class c \
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = to_regclass($1)";

/// `SQLite`: the first of `temp`, `main` and the attached databases (in attach order) that holds
/// the name.
const SQLITE_SQL: &str = "SELECT t.schema FROM pragma_table_list AS t \
    JOIN pragma_database_list AS d ON d.name = t.schema WHERE t.name = ?1 COLLATE NOCASE \
    ORDER BY CASE t.schema WHEN 'temp' THEN 0 WHEN 'main' THEN 1 ELSE 2 END, d.seq LIMIT 1";

/// `MySQL` and `MariaDB`: the current database (a temporary table shadows by name, not by schema).
///
/// The driver prepares every statement and the connection caches the prepared form by its text,
/// and a cached `SELECT DATABASE()` keeps answering with the database it was first prepared in,
/// even after a `USE`. So each probe carries a text of its own (a numbered comment) and is
/// never served from that cache. (A `USE` is run by the driver over the text protocol, because
/// MariaDB ignores a repeated prepared `USE`.)
const MYSQL_SQL: &str = "SELECT DATABASE()";

/// Numbers the `MySQL` probe texts, so no two share a cache entry.
static MYSQL_PROBES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The lookup of the schema that an unqualified table name of one query resolves to.
#[derive(Clone, PartialEq, Eq)]
pub struct SchemaProbe {
    dialect: Dialect,
    /// The bound parameter; empty for `MySQL`, which binds nothing.
    name: String,
}

impl std::fmt::Debug for SchemaProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaProbe")
            .field("dialect", &self.dialect)
            .finish_non_exhaustive()
    }
}

impl SchemaProbe {
    /// The probe for the query `sql`, or `None` unless `single_table_source` shows a plain
    /// `SELECT` from one **unqualified** table (a qualified name is compared with the table's own
    /// schema instead, no lookup needed).
    #[must_use]
    pub fn for_sql(dialect: Dialect, sql: &str) -> Option<Self> {
        let source = single_table_source(dialect, sql)?;
        if source.schema.is_some() || source.name.text.is_empty() {
            return None;
        }
        let name = match dialect {
            Dialect::Postgres if source.name.quoted => {
                quote_identifier(dialect, &source.name.text).ok()?
            }
            // As written: `to_regclass` folds an unquoted name the way the query did. The lexer
            // only yields identifier characters here; refuse anything else rather than let the
            // server parse it as something else.
            Dialect::Postgres => {
                let plain = source
                    .name
                    .text
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
                if !plain {
                    return None;
                }
                source.name.text.clone()
            }
            Dialect::MySql => String::new(),
            _ => source.name.text.clone(),
        };
        Some(Self { dialect, name })
    }

    /// The statement and its one bound parameter (none for `MySQL`). The name is only ever a
    /// parameter; the SQL text is fixed (a numbered comment on `MySQL`).
    #[must_use]
    pub fn query(&self) -> Query {
        match self.dialect {
            Dialect::Postgres => {
                Query::new(PG_SQL).with_params(vec![Value::Text(self.name.clone())])
            }
            Dialect::MySql => {
                let n = MYSQL_PROBES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Query::new(format!("{MYSQL_SQL} /* sqler-probe {n} */"))
            }
            _ => Query::new(SQLITE_SQL).with_params(vec![Value::Text(self.name.clone())]),
        }
    }

    /// Runs the lookup on `session`; any error, a missing row or a `NULL` gives `None`.
    pub fn run<'a>(&'a self, session: &'a mut dyn Session) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let query = self.query();
            let mut stream = session.execute(&query).await.ok()?;
            let result = collect(&mut *stream).await.ok()?;
            match result.rows().first()?.get(0)? {
                Value::Text(schema) if !schema.is_empty() => Some(schema.clone()),
                _ => None,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::DriverRegistry;
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};
    use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};

    use super::*;

    fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    async fn session(url: &str) -> Box<dyn Session> {
        let mut registry = DriverRegistry::new();
        registry.register(Box::new(SqliteDriver::new())).unwrap();
        let config = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), url);
        registry.connect(&config).await.unwrap()
    }

    async fn exec(s: &mut dyn Session, sql: &str) {
        let q = Query::new(sql);
        let mut stream = s.execute(&q).await.unwrap();
        collect(&mut *stream).await.unwrap();
    }

    async fn resolve(s: &mut dyn Session, sql: &str) -> Option<String> {
        probe(Dialect::Sqlite, sql).unwrap().run(s).await
    }

    fn probe(d: Dialect, sql: &str) -> Option<SchemaProbe> {
        SchemaProbe::for_sql(d, sql)
    }

    #[test]
    fn only_an_unqualified_single_table_select_gets_a_probe() {
        let l = Dialect::Sqlite;
        assert!(probe(l, "SELECT * FROM t").is_some());
        assert!(probe(l, "select id, name from t where id = 1 limit 5").is_some());
        assert!(probe(l, "SELECT * FROM main.t").is_none());
        assert!(probe(l, "SELECT * FROM t JOIN u ON 1 = 1").is_none());
        assert!(probe(l, "SELECT * FROM t; SELECT * FROM u").is_none());
        assert!(probe(l, "UPDATE t SET a = 1").is_none());
        assert!(probe(l, "DELETE FROM t").is_none());
        assert!(probe(l, "").is_none());
        assert!(probe(Dialect::Postgres, "SELECT * FROM public.t").is_none());
        assert!(probe(Dialect::MySql, "SELECT * FROM db.t").is_none());
    }

    #[test]
    fn postgres_binds_the_name_as_written() {
        let p = probe(Dialect::Postgres, "SELECT * FROM Foo").unwrap();
        let q = p.query();
        assert_eq!(q.sql, PG_SQL);
        assert_eq!(q.params, vec![Value::Text("Foo".to_owned())]);
        let p = probe(Dialect::Postgres, "SELECT * FROM \"Mixed \"\"Q\"\" x\"").unwrap();
        assert_eq!(
            p.query().params,
            vec![Value::Text("\"Mixed \"\"Q\"\" x\"".to_owned())]
        );
    }

    #[test]
    fn mysql_binds_nothing() {
        let q = probe(Dialect::MySql, "SELECT * FROM `t`").unwrap().query();
        assert!(q.sql.starts_with(MYSQL_SQL), "{}", q.sql);
        let again = probe(Dialect::MySql, "SELECT * FROM `t`").unwrap().query();
        assert_ne!(q.sql, again.sql, "each probe has a text of its own");
        assert_eq!(q.params.len(), 0);
    }

    /// The statement text is one of three constants whatever the table is called, the name is
    /// a bound parameter, and every statement is a proven read.
    #[test]
    fn the_name_is_never_spliced_and_the_statements_are_reads() {
        let names = [
            "t",
            "T1",
            "a_b$c",
            "\"x'; DROP TABLE t; --\"",
            "\"has \"\"quote\"\"\"",
            "\"semi;colon\"",
            "`back``tick`",
            "\"naïve\"",
            "\"?1 $1 ?\"",
            "\"/* c */\"",
        ];
        for d in [Dialect::Postgres, Dialect::MySql, Dialect::Sqlite] {
            for n in names {
                let Some(p) = probe(d, &format!("SELECT * FROM {n}")) else {
                    continue;
                };
                let q = p.query();
                assert!(
                    q.sql == PG_SQL || q.sql == SQLITE_SQL || q.sql.starts_with(MYSQL_SQL),
                    "{d:?} {n}"
                );
                assert!(!q.sql.contains(';'), "{d:?} {n}");
                assert_eq!(
                    q.params.len(),
                    usize::from(d != Dialect::MySql),
                    "{d:?} {n}"
                );
                // The classifier cannot prove table functions and `to_regclass` pure, so the
                // three constants are checked by shape: one SELECT, no writing keyword.
                let upper = q.sql.to_ascii_uppercase();
                assert!(upper.starts_with("SELECT "), "{d:?}");
                for word in [
                    "INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "ALTER", "PRAGMA ", "SET ",
                    "ATTACH",
                ] {
                    assert!(!upper.contains(word), "{d:?} {word}");
                }
            }
        }
    }

    #[test]
    fn debug_shows_no_name() {
        let p = probe(Dialect::Sqlite, "SELECT * FROM sekrit_table").unwrap();
        assert!(!format!("{p:?}").contains("sekrit"));
    }

    #[test]
    fn sqlite_resolves_main_temp_attached_and_missing() {
        block_on(async {
            let dir = std::env::temp_dir().join(format!("sqler-probe-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let other = dir.join("other.db");
            let _ = std::fs::remove_file(&other);
            let mut s = session(":memory:").await;
            let s = &mut *s;
            exec(s, "CREATE TABLE t (id INTEGER)").await;
            exec(s, "CREATE TABLE only_main (id INTEGER)").await;
            assert_eq!(resolve(s, "SELECT * FROM t").await.as_deref(), Some("main"));
            assert_eq!(resolve(s, "SELECT * FROM T").await.as_deref(), Some("main"));
            assert_eq!(
                resolve(s, "SELECT * FROM only_main").await.as_deref(),
                Some("main")
            );
            // A missing name.
            assert_eq!(resolve(s, "SELECT * FROM nope").await, None);

            // A temp table shadows main.
            exec(s, "CREATE TEMP TABLE t (id INTEGER)").await;
            assert_eq!(resolve(s, "SELECT * FROM t").await.as_deref(), Some("temp"));
            assert_eq!(
                resolve(s, "SELECT * FROM only_main").await.as_deref(),
                Some("main")
            );

            // An attached database holds a name main does not.
            exec(
                s,
                &format!("ATTACH DATABASE '{}' AS other", other.display()),
            )
            .await;
            exec(s, "CREATE TABLE other.extra (id INTEGER)").await;
            assert_eq!(
                resolve(s, "SELECT * FROM extra").await.as_deref(),
                Some("other")
            );
            // main wins over an attached database with the same name.
            exec(s, "CREATE TABLE other.only_main (id INTEGER)").await;
            assert_eq!(
                resolve(s, "SELECT * FROM only_main").await.as_deref(),
                Some("main")
            );
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn a_failing_lookup_is_none() {
        block_on(async {
            // A probe whose dialect does not match the server fails to prepare: None.
            let mut s = session(":memory:").await;
            let p = probe(Dialect::Postgres, "SELECT * FROM t").unwrap();
            assert_eq!(p.run(&mut *s).await, None);
            let p = probe(Dialect::MySql, "SELECT * FROM t").unwrap();
            assert_eq!(p.run(&mut *s).await, None);
        });
    }
}
