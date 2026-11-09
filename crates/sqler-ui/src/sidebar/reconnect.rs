// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The confirmation shown before a reconnect that would lose a transaction or a running query.

use sqler_config::ConnectionId;

/// What the reconnect confirmation says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectPrompt {
    /// The connection to reconnect.
    pub conn: ConnectionId,
    /// Its display name.
    pub name: String,
    /// Tabs that are inside an open transaction.
    pub open_transactions: usize,
    /// Tabs with a running query.
    pub running: usize,
}

impl ReconnectPrompt {
    /// The dialog title.
    #[must_use]
    pub fn title(&self) -> String {
        format!("Reconnect \"{}\"?", self.name)
    }

    /// The warning lines of the dialog: one per risk that applies.
    #[must_use]
    pub fn prompt_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.open_transactions > 0 {
            let n = self.open_transactions;
            let noun = if n == 1 { "tab is" } else { "tabs are" };
            lines.push(format!(
                "{n} {noun} inside an open transaction. Reconnecting closes those sessions; \
                 the transaction and its uncommitted changes are lost."
            ));
        }
        if self.running > 0 {
            let n = self.running;
            let noun = if n == 1 { "query" } else { "queries" };
            lines.push(format!("{n} running {noun} will be cancelled."));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::{ConnectionSpec, ConnectionStore};
    use sqler_driver_api::sqler_core::DriverId;

    fn prompt(open_transactions: usize, running: usize) -> ReconnectPrompt {
        let mut store = ConnectionStore::new();
        let conn = store
            .add_connection(
                ConnectionSpec::new("c", DriverId::new("sqlite").unwrap(), "x"),
                None,
            )
            .unwrap();
        ReconnectPrompt {
            conn,
            name: "dev".into(),
            open_transactions,
            running,
        }
    }

    #[test]
    fn title_names_the_connection() {
        assert_eq!(prompt(0, 0).title(), "Reconnect \"dev\"?");
    }

    #[test]
    fn singular_text() {
        let lines = prompt(1, 1).prompt_lines();
        assert_eq!(
            lines,
            vec![
                "1 tab is inside an open transaction. Reconnecting closes those sessions; \
                 the transaction and its uncommitted changes are lost."
                    .to_owned(),
                "1 running query will be cancelled.".to_owned(),
            ]
        );
    }

    #[test]
    fn plural_text() {
        let lines = prompt(2, 3).prompt_lines();
        assert!(lines[0].starts_with("2 tabs are inside an open transaction."));
        assert_eq!(lines[1], "3 running queries will be cancelled.");
    }

    #[test]
    fn a_line_appears_only_when_its_count_is_positive() {
        assert_eq!(prompt(0, 0).prompt_lines(), Vec::<String>::new());
        let tx = prompt(1, 0).prompt_lines();
        assert_eq!(tx.len(), 1);
        assert!(tx[0].contains("transaction"));
        let run = prompt(0, 2).prompt_lines();
        assert_eq!(run.len(), 1);
        assert!(run[0].contains("running"));
    }
}
