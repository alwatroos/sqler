// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SidebarModel`: the saved connections, the selection and per-entry connect status.

use sqler_driver_api::sqler_core::ConnectionConfig;

/// Where a connection stands in the connect flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// No connect attempt is running and the last one (if any) succeeded.
    Idle,
    /// At least one connect attempt is running.
    Connecting,
    /// The last connect attempt failed with this message.
    Failed(String),
}

/// One row of the sidebar.
#[derive(Debug, Clone)]
pub struct SidebarEntry {
    config: ConnectionConfig,
    pending: usize,
    status: ConnectionStatus,
}

impl SidebarEntry {
    /// The saved connection.
    #[must_use]
    pub fn config(&self) -> &ConnectionConfig {
        &self.config
    }

    /// The connect status.
    #[must_use]
    pub fn status(&self) -> &ConnectionStatus {
        &self.status
    }
}

/// The sidebar state: connections in file order, the selection and a load error.
#[derive(Debug, Clone, Default)]
pub struct SidebarModel {
    entries: Vec<SidebarEntry>,
    selected: Option<usize>,
    load_error: Option<String>,
}

impl SidebarModel {
    /// Builds the model from the saved connections, keeping their order.
    #[must_use]
    pub fn from_connections(connections: Vec<ConnectionConfig>) -> Self {
        Self {
            entries: connections
                .into_iter()
                .map(|config| SidebarEntry {
                    config,
                    pending: 0,
                    status: ConnectionStatus::Idle,
                })
                .collect(),
            selected: None,
            load_error: None,
        }
    }

    /// An empty model that shows why the connection list could not be loaded.
    #[must_use]
    pub fn with_load_error(message: String) -> Self {
        Self {
            load_error: Some(message),
            ..Self::default()
        }
    }

    /// The entries in file order.
    #[must_use]
    pub fn entries(&self) -> &[SidebarEntry] {
        &self.entries
    }

    /// The selected entry index.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The error from loading the connection list, if any.
    #[must_use]
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Selects an entry; returns `false` (and changes nothing) when `index` is out of range.
    pub fn select(&mut self, index: usize) -> bool {
        if index < self.entries.len() {
            self.selected = Some(index);
            true
        } else {
            false
        }
    }

    /// Marks one more connect attempt as running for `index`.
    pub(crate) fn begin_connect(&mut self, index: usize) -> bool {
        match self.entries.get_mut(index) {
            Some(entry) => {
                entry.pending += 1;
                entry.status = ConnectionStatus::Connecting;
                true
            }
            None => false,
        }
    }

    /// Records the end of one connect attempt for `index`.
    pub(crate) fn finish_connect(&mut self, index: usize, result: Result<(), String>) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.pending = entry.pending.saturating_sub(1);
            // Status is `Connecting` while any attempt is still running.
            if entry.pending == 0 {
                entry.status = match result {
                    Ok(()) => ConnectionStatus::Idle,
                    Err(msg) => ConnectionStatus::Failed(msg),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::DriverId;

    fn cfg(name: &str) -> ConnectionConfig {
        ConnectionConfig::new(name, DriverId::new("sqlite").unwrap(), ":memory:")
    }

    fn model() -> SidebarModel {
        SidebarModel::from_connections(vec![cfg("b"), cfg("a")])
    }

    #[test]
    fn keeps_file_order() {
        let m = model();
        let names: Vec<_> = m
            .entries()
            .iter()
            .map(|e| e.config().name.as_str())
            .collect();
        assert_eq!(names, ["b", "a"]);
        assert_eq!(m.selected(), None);
        assert_eq!(m.load_error(), None);
    }

    #[test]
    fn select_checks_bounds() {
        let mut m = model();
        assert!(m.select(1));
        assert_eq!(m.selected(), Some(1));
        assert!(!m.select(2));
        assert_eq!(m.selected(), Some(1));
    }

    #[test]
    fn status_transitions() {
        let mut m = model();
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Idle);
        assert!(m.begin_connect(0));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Connecting);
        m.finish_connect(0, Err("boom".into()));
        assert_eq!(
            *m.entries()[0].status(),
            ConnectionStatus::Failed("boom".into())
        );
        assert!(m.begin_connect(0));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Connecting);
        m.finish_connect(0, Ok(()));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Idle);
        assert_eq!(*m.entries()[1].status(), ConnectionStatus::Idle);
    }

    #[test]
    fn pending_counter_keeps_connecting() {
        let mut m = model();
        m.begin_connect(0);
        m.begin_connect(0);
        m.finish_connect(0, Ok(()));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Connecting);
        m.finish_connect(0, Ok(()));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Idle);
    }

    #[test]
    fn out_of_range_connect_is_ignored() {
        let mut m = model();
        assert!(!m.begin_connect(9));
        m.finish_connect(9, Ok(()));
    }

    #[test]
    fn load_error_is_kept() {
        let m = SidebarModel::with_load_error("bad file".into());
        assert_eq!(m.load_error(), Some("bad file"));
        assert!(m.entries().is_empty());
    }
}
