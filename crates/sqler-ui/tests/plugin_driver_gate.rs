// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The production gate for plugin drivers: a driver that declares a dialect (SQL Server: `tsql`) gets the
//! read-only fast path of its dialect, a driver without one (`DuckDB`) asks for every statement.

// Helpers outside `#[test]` fns also expect; clippy.toml only exempts the test fns themselves.
#![allow(clippy::expect_used)]

use sqler_sql::Dialect;
use sqler_ui::query::{BufferSnapshot, Plan, QueryTab, RunMode, Step, plan};

fn planned(dialect: Option<Dialect>, text: &str) -> Plan {
    plan(dialect, &BufferSnapshot::at_cursor(text, 0), RunMode::All).expect("a plan")
}

fn step(dialect: Option<Dialect>, text: &str, prod: bool) -> Step {
    let mut tab = QueryTab::new();
    tab.request(dialect, planned(dialect, text), prod)
        .expect("a step")
}

/// What the manifest of the SQL Server plugin declares (`dialect = "tsql"`), resolved by name.
fn sqlserver() -> Option<Dialect> {
    Dialect::from_name("tsql")
}

#[test]
fn the_declared_name_gives_t_sql_and_the_driver_id_alone_gives_none() {
    assert_eq!(sqlserver(), Some(Dialect::TSql));
    assert_eq!(Dialect::from_driver_id("sqlserver"), None);
}

#[test]
fn prod_starts_a_select_without_asking() {
    let step = step(sqlserver(), "select 1", true);
    assert!(matches!(step, Step::Start(_)), "{step:?}");
}

#[test]
fn prod_confirms_every_statement_that_is_not_read() {
    for sql in [
        "insert into t values (1)",
        "update t set a = 1",
        "exec p",
        "execute('drop table t')",
        "select 1\nGO\ndelete from t",
        "declare @x int = 1",
    ] {
        let step = step(sqlserver(), sql, true);
        assert!(matches!(step, Step::Confirm), "{sql}: {step:?}");
    }
}

#[test]
fn non_prod_starts_a_write() {
    let step = step(sqlserver(), "insert into t values (1)", false);
    assert!(matches!(step, Step::Start(_)), "{step:?}");
}

#[test]
fn a_driver_without_a_dialect_confirms_even_a_select() {
    let step = step(None, "select 1", true);
    assert!(matches!(step, Step::Confirm), "{step:?}");
}
