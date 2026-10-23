// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Error mapping. `DuckDB` messages can contain file paths, so they are scrubbed.

use sqler_driver_api::DriverError;

/// The fixed connect text for a database file another process holds.
pub(crate) const LOCKED: &str = "the DuckDB database file is locked by another process";
/// The fixed connect text for every other open failure.
pub(crate) const NOT_OPENED: &str = "the DuckDB database could not be opened";

/// Replaces every path the session was opened with by `<database>`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Scrub {
    needles: Vec<String>,
}

impl Scrub {
    /// Builds a scrubber for the target as given and its canonical form.
    pub(crate) fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let mut needles: Vec<String> = paths.into_iter().filter(|p| !p.is_empty()).collect();
        needles.sort_by_key(|p| std::cmp::Reverse(p.len()));
        needles.dedup();
        Self { needles }
    }

    pub(crate) fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for needle in &self.needles {
            out = out.replace(needle.as_str(), "<database>");
        }
        out
    }
}

/// Maps an open failure to a fixed text that never contains the path.
pub(crate) fn connect_error(err: &duckdb::Error) -> DriverError {
    let text = err.to_string().to_ascii_lowercase();
    DriverError::Connection(
        if text.contains("could not set lock") || text.contains("conflicting lock") {
            LOCKED
        } else {
            NOT_OPENED
        }
        .to_owned(),
    )
}

/// Maps a prepare, execute or fetch failure, passing `DuckDB`'s text through scrubbed.
pub(crate) fn query_error(err: &duckdb::Error, scrub: &Scrub) -> DriverError {
    DriverError::Query(scrub.apply(&err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_replaces_all_forms() {
        let s = Scrub::new(["/a/b.db".to_owned(), "/a/b.db.wal".to_owned()]);
        assert_eq!(
            s.apply("x /a/b.db.wal y /a/b.db z"),
            "x <database> y <database> z"
        );
    }

    #[test]
    fn empty_scrub_is_identity() {
        assert_eq!(Scrub::new([String::new()]).apply("abc"), "abc");
    }
}
