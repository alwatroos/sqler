// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The production gate for a plugin driver without a dialect (SQL Server): no statement counts as read-only.

// Helpers outside `#[test]` fns also expect; clippy.toml only exempts the test fns themselves.
#![allow(clippy::expect_used)]

use sqler_sql::Dialect;
use sqler_ui::query::{BufferSnapshot, Plan, QueryTab, RunMode, Step, plan};

fn planned(dialect: Option<Dialect>, text: &str) -> Plan {
    plan(dialect, &BufferSnapshot::at_cursor(text, 0), RunMode::All).expect("a plan")
}

#[test]
fn sqlserver_has_no_dialect() {
    assert_eq!(Dialect::from_driver_id("sqlserver"), None);
}

#[test]
fn prod_confirms_a_write() {
    let dialect = Dialect::from_driver_id("sqlserver");
    let mut tab = QueryTab::new();
    let step = tab
        .request(dialect, planned(dialect, "insert into t values (1)"), true)
        .expect("a step");
    assert!(matches!(step, Step::Confirm), "{step:?}");
}

#[test]
fn prod_confirms_even_a_select() {
    let dialect = Dialect::from_driver_id("sqlserver");
    let mut tab = QueryTab::new();
    let step = tab
        .request(dialect, planned(dialect, "select 1"), true)
        .expect("a step");
    assert!(matches!(step, Step::Confirm), "{step:?}");
}

#[test]
fn non_prod_starts_a_write() {
    let dialect = Dialect::from_driver_id("sqlserver");
    let mut tab = QueryTab::new();
    let step = tab
        .request(dialect, planned(dialect, "insert into t values (1)"), false)
        .expect("a step");
    assert!(matches!(step, Step::Start(_)), "{step:?}");
}
