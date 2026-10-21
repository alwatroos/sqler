// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What the welcome view shows: whether there are connections, whether an editor can be opened
//! and the recently opened connections.

use sqler_driver_api::sqler_core::Environment;

use super::recent::RecentConnections;
use crate::sidebar::SidebarModel;
use crate::workspace::Workspace;

/// The most recent connections the welcome view lists.
pub const MAX_WELCOME_RECENTS: usize = 5;

/// A recently opened connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentItem {
    /// The connection's index in the sidebar (what `OpenConnection` takes).
    pub index: usize,
    /// The connection name.
    pub name: String,
    /// The environment tag.
    pub env: Option<Environment>,
}

/// Everything the welcome view renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WelcomeModel {
    /// Whether at least one connection is saved.
    pub has_connections: bool,
    /// Whether "New SQL Editor" has a connection to open (one is selected).
    pub can_new_editor: bool,
    /// Up to [`MAX_WELCOME_RECENTS`] recent connections, most recent first.
    pub recent: Vec<RecentItem>,
}

/// The welcome model of a workspace.
#[must_use]
pub fn welcome_model(ws: &Workspace) -> WelcomeModel {
    model_of(ws.sidebar(), ws.recent_connections())
}

fn model_of(sidebar: &SidebarModel, recent: &RecentConnections) -> WelcomeModel {
    let recent = recent
        .iter()
        .filter_map(|id| {
            let index = sidebar.index_of(id)?;
            let config = sidebar.entries().get(index)?.config();
            Some(RecentItem {
                index,
                name: config.name.clone(),
                env: config.environment,
            })
        })
        .take(MAX_WELCOME_RECENTS)
        .collect();
    WelcomeModel {
        has_connections: !sidebar.entries().is_empty(),
        can_new_editor: sidebar.selected().is_some(),
        recent,
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

    use super::*;
    use crate::workspace::WorkspaceAction;

    fn sidebar(n: usize) -> SidebarModel {
        let driver = DriverId::new("sqlite").unwrap();
        SidebarModel::from_connections(
            (0..n)
                .map(|i| {
                    ConnectionConfig::new(format!("c{i}"), driver.clone(), ":memory:")
                        .with_environment(Environment::Prod)
                })
                .collect(),
        )
    }

    fn recents(sidebar: &SidebarModel, indexes: &[usize]) -> RecentConnections {
        let mut r = RecentConnections::default();
        for i in indexes {
            r.record(sidebar.entries()[*i].id());
        }
        r
    }

    #[test]
    fn no_connections() {
        let s = sidebar(0);
        let m = model_of(&s, &RecentConnections::default());
        assert!(!m.has_connections);
        assert!(!m.can_new_editor);
        assert_eq!(m.recent, []);
    }

    #[test]
    fn recents_are_most_recent_first_with_env() {
        let s = sidebar(3);
        let m = model_of(&s, &recents(&s, &[0, 2]));
        assert!(m.has_connections);
        let got: Vec<_> = m
            .recent
            .iter()
            .map(|r| (r.index, r.name.as_str()))
            .collect();
        assert_eq!(got, [(2, "c2"), (0, "c0")]);
        assert_eq!(m.recent[0].env, Some(Environment::Prod));
    }

    #[test]
    fn seven_recents_are_cut_to_five() {
        let s = sidebar(7);
        let m = model_of(&s, &recents(&s, &[0, 1, 2, 3, 4, 5, 6]));
        assert_eq!(m.recent.len(), MAX_WELCOME_RECENTS);
        assert_eq!(m.recent[0].index, 6);
    }

    #[test]
    fn deleted_connection_is_skipped() {
        let mut s = sidebar(3);
        let r = recents(&s, &[0, 1, 2]);
        let gone = s.entries()[1].id().clone();
        s.delete_connection(&gone).unwrap();
        let m = model_of(&s, &r);
        let got: Vec<_> = m.recent.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(got, ["c2", "c0"]);
        assert_eq!(m.recent[0].index, 1);
    }

    #[test]
    fn can_new_editor_follows_the_selection() {
        let mut ws = Workspace::new(sidebar(2));
        assert!(!welcome_model(&ws).can_new_editor);
        ws.dispatch(WorkspaceAction::SelectConnection(1));
        assert!(welcome_model(&ws).can_new_editor);
    }
}
