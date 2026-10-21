// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The status bar model and its text helpers.

use std::time::Duration;

use sqler_driver_api::sqler_core::{DriverId, Environment};

use crate::grid::group_thousands;
use crate::tabs::Rgb;

/// What the status bar shows about the run state of the active tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunIndicator {
    /// Nothing is running.
    #[default]
    Idle,
    /// A statement is running.
    Running,
    /// A cancel was requested and has not finished yet.
    Cancelling,
}

/// Row counts of the result in view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowInfo {
    /// Rows loaded so far.
    pub loaded: u64,
    /// Rows shown (after a filter); at most `loaded`.
    pub shown: u64,
    /// Whether the server has more rows than `loaded`.
    pub more: bool,
}

/// Everything the status bar renders. `None` fields are hidden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusBarModel {
    /// The environment of the active tab's connection.
    pub env: Option<Environment>,
    /// The connection name.
    pub connection: Option<String>,
    /// The connection colour.
    pub accent: Option<Rgb>,
    /// The database name; never a user, host or password.
    pub database: Option<String>,
    /// The run state.
    pub run: RunIndicator,
    /// The row count text, see [`format_rows`].
    pub rows: Option<String>,
    /// The duration of the statement in view.
    pub timing: Option<String>,
}

/// `1 row`, `1,234 rows`, `500 of 1,234 rows`, `100,000+ rows`.
#[must_use]
pub fn format_rows(info: RowInfo) -> String {
    let plus = if info.more { "+" } else { "" };
    let loaded = group_thousands(usize::try_from(info.loaded).unwrap_or(usize::MAX));
    if info.shown < info.loaded {
        let shown = group_thousands(usize::try_from(info.shown).unwrap_or(usize::MAX));
        format!("{shown} of {loaded}{plus} rows")
    } else if info.more {
        format!("{loaded}+ rows")
    } else if info.loaded == 1 {
        "1 row".to_owned()
    } else {
        format!("{loaded} rows")
    }
}

/// `12 ms` below one second, `1.2 s` from then on (the rule of the statement chips).
// Used by the workspace wiring (U2 W3) and the shell; no caller inside this package.
#[allow(dead_code)]
pub(crate) fn format_duration(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{} ms", d.as_millis())
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

/// The database to show for a connection: a file name, `memory`, or the database part of a
/// URL. `None` when it cannot be told safely. A user, password or host is never returned.
#[must_use]
pub fn database_label(driver: &DriverId, target: &str) -> Option<String> {
    let label = if matches!(driver.as_str(), "sqlite" | "duckdb") {
        file_label(target)
    } else {
        url_label(target)
    }?;
    (!label.is_empty() && !label.contains(['@', ':'])).then_some(label)
}

fn file_label(target: &str) -> Option<String> {
    let t = target.trim();
    let t = t.strip_prefix("file:").unwrap_or(t);
    let t = t.split('?').next().unwrap_or("");
    if t.is_empty() || t == ":memory:" {
        return Some("memory".to_owned());
    }
    t.rsplit(['/', '\\']).next().map(str::to_owned)
}

fn url_label(target: &str) -> Option<String> {
    if !target.contains("://") {
        return None;
    }
    let (_, rest) = target.split_once("://")?;
    // Userinfo first: a raw password may hold `/`, `?` and `#`.
    let rest = rest.rsplit_once('@').map_or(rest, |(_, after)| after);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (_, path) = rest.split_once('/')?;
    path.split('/').next().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rows(loaded: u64, shown: u64, more: bool) -> String {
        format_rows(RowInfo {
            loaded,
            shown,
            more,
        })
    }

    #[test]
    fn rows_examples() {
        assert_eq!(rows(0, 0, false), "0 rows");
        assert_eq!(rows(1, 1, false), "1 row");
        assert_eq!(rows(2, 2, false), "2 rows");
        assert_eq!(rows(1234, 1234, false), "1,234 rows");
        assert_eq!(rows(1234, 500, false), "500 of 1,234 rows");
        assert_eq!(rows(100_000, 100_000, true), "100,000+ rows");
        assert_eq!(rows(1000, 10, true), "10 of 1,000+ rows");
        assert_eq!(rows(1, 1, true), "1+ rows");
    }

    #[test]
    fn duration_rule() {
        assert_eq!(format_duration(Duration::from_millis(12)), "12 ms");
        assert_eq!(format_duration(Duration::from_millis(999)), "999 ms");
        assert_eq!(format_duration(Duration::from_millis(1200)), "1.2 s");
    }

    fn label(driver: &str, target: &str) -> Option<String> {
        database_label(&DriverId::new(driver).unwrap(), target)
    }

    #[test]
    fn file_drivers() {
        for d in ["sqlite", "duckdb"] {
            assert_eq!(label(d, ""), Some("memory".into()));
            assert_eq!(label(d, ":memory:"), Some("memory".into()));
            assert_eq!(
                label(d, "file::memory:?cache=shared"),
                Some("memory".into())
            );
            assert_eq!(label(d, "/home/u/data/app.db"), Some("app.db".into()));
            assert_eq!(label(d, "C:\\data\\app.db"), Some("app.db".into()));
            assert_eq!(label(d, "file:/tmp/x.db?mode=ro"), Some("x.db".into()));
            assert_eq!(label(d, "app.db"), Some("app.db".into()));
            assert_eq!(label(d, "/dir/"), None);
        }
    }

    #[test]
    fn url_drivers() {
        assert_eq!(
            label("postgres", "postgres://u:p@h:5432/shop"),
            Some("shop".into())
        );
        assert_eq!(
            label("postgres", "postgres://u:p@h/shop?sslmode=require"),
            Some("shop".into())
        );
        assert_eq!(label("mysql", "mysql://h/shop/extra"), Some("shop".into()));
        assert_eq!(label("postgres", "postgres://u:p@h"), None);
        assert_eq!(label("postgres", "postgres://u:p@h/"), None);
        assert_eq!(label("postgres", "host=h dbname=shop"), None);
        assert_eq!(label("postgres", ""), None);
        assert_eq!(
            label("postgres", "postgres://u:p/ss@h/shop"),
            Some("shop".into())
        );
        assert_eq!(label("postgres", "postgres://u@h/db@x"), None);
    }

    proptest! {
        #[test]
        fn label_never_leaks(
            user in "[A-Z0-9]{4,12}",
            pass in "[A-Z0-9/:@?#]{4,12}",
            host in "[A-Z0-9]{4,12}",
            db in "[a-z]{1,12}",
        ) {
            let target = format!("postgres://{user}:{pass}@{host}:5432/{db}?sslmode=require");
            let got = label("postgres", &target);
            if let Some(l) = &got {
                prop_assert!(!l.contains(&user) && !l.contains(&pass) && !l.contains(&host));
                prop_assert!(!l.contains('@'));
            }
            if !pass.contains('/') {
                prop_assert_eq!(got, Some(db));
            }
        }
    }
}
