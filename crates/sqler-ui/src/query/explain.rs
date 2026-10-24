// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `query:explain`: the statement text that asks the server for a plan.
//!
//! The wrapped text goes through the normal run pipeline, so the production confirmation of
//! ADR-0024 still applies (`EXPLAIN <write>` is classified as the write). `ANALYZE` is never
//! added, so on `PostgreSQL`, `MySQL`, `SQLite` and `DuckDB` the statement is planned, not executed.

use sqler_sql::Dialect;

/// The notice for a connection whose driver has no explain form.
pub const EXPLAIN_UNSUPPORTED: &str = "Explain is not available for this connection's driver.";

/// `sql` prefixed with the dialect's explain keyword; nothing else changes.
#[must_use]
pub fn explain_text(dialect: Dialect, sql: &str) -> Option<String> {
    let prefix = match dialect {
        Dialect::Postgres | Dialect::MySql => "EXPLAIN ",
        Dialect::Sqlite => "EXPLAIN QUERY PLAN ",
        _ => return None,
    };
    Some(format!("{prefix}{sql}"))
}

/// The explain text for a driver: by dialect when there is one, `DuckDB` by its driver ID.
#[must_use]
pub fn explain_for(driver: &str, dialect: Option<Dialect>, sql: &str) -> Option<String> {
    match dialect {
        Some(d) => explain_text(d, sql),
        None if driver == "duckdb" => Some(format!("EXPLAIN {sql}")),
        None => None,
    }
}

/// Whether the driver has an explain form.
#[must_use]
pub fn explain_supported(driver: &str, dialect: Option<Dialect>) -> bool {
    explain_for(driver, dialect, "x").is_some()
}

#[cfg(test)]
mod tests {
    use sqler_sql::{StatementClass, classify};

    use super::*;

    #[test]
    fn exact_text_per_dialect() {
        assert_eq!(
            explain_text(Dialect::Postgres, "select 1").as_deref(),
            Some("EXPLAIN select 1")
        );
        assert_eq!(
            explain_text(Dialect::MySql, "select 1").as_deref(),
            Some("EXPLAIN select 1")
        );
        assert_eq!(
            explain_text(Dialect::Sqlite, "select 1").as_deref(),
            Some("EXPLAIN QUERY PLAN select 1")
        );
    }

    #[test]
    fn by_driver() {
        assert_eq!(
            explain_for("duckdb", None, "select 1").as_deref(),
            Some("EXPLAIN select 1")
        );
        assert_eq!(explain_for("mssql", None, "select 1"), None);
        assert_eq!(explain_for("oracle", None, "select 1"), None);
        assert_eq!(
            explain_for("sqlite", Some(Dialect::Sqlite), "select 1").as_deref(),
            Some("EXPLAIN QUERY PLAN select 1")
        );
        assert!(explain_supported("duckdb", None));
        assert!(explain_supported("postgres", Some(Dialect::Postgres)));
        assert!(!explain_supported("mssql", None));
    }

    #[test]
    fn never_adds_analyze() {
        for d in Dialect::ALL {
            let Some(text) = explain_text(d, "select 1") else {
                assert_eq!(d, Dialect::TSql);
                continue;
            };
            assert!(!text.to_uppercase().contains("ANALYZE"), "{text}");
        }
        assert!(
            !explain_for("duckdb", None, "select 1")
                .unwrap()
                .contains("ANALYZE")
        );
    }

    #[test]
    fn the_gate_still_sees_the_write() {
        for d in [Dialect::Postgres, Dialect::Sqlite, Dialect::MySql] {
            let write = explain_text(d, "delete from t").unwrap();
            assert_eq!(classify(d, &write), StatementClass::Write, "{write}");
            let read = explain_text(d, "select 1").unwrap();
            assert_eq!(classify(d, &read), StatementClass::Read, "{read}");
        }
    }
}
