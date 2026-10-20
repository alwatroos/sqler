// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `TabStrip`: ordered tabs, each bound to one connection.

use std::collections::HashMap;

use sqler_driver_api::sqler_core::{DriverId, Environment};

use super::header::{Rgb, TabHeader};

/// Stable identifier of a tab; never reused within one strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(u64);

impl TabId {
    /// The raw number, for keying UI elements.
    #[cfg(feature = "gui")]
    pub(crate) fn raw(self) -> u64 {
        self.0
    }
}

/// What a tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TabKind {
    /// A SQL editor tab: one connection with its own session and a code editor.
    Query,
    /// A results grid.
    Grid,
    /// The generated DDL or script of a database object (read-only text).
    Ddl,
    /// The properties of a database object or column (read-only table).
    Properties,
}

/// The connection data a tab needs to render its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabConnection {
    /// The connection name.
    pub name: String,
    /// The driver id.
    pub driver: DriverId,
    /// The accent colour as `#rrggbb`.
    pub color: Option<String>,
    /// The environment tag.
    pub environment: Option<Environment>,
}

/// One tab.
#[derive(Debug, Clone)]
pub struct Tab {
    id: TabId,
    connection: TabConnection,
    kind: TabKind,
    ordinal: u32,
    /// The database object an object tab shows (for the title).
    object: Option<String>,
}

/// The badge text for an environment, shared by the sidebar and tab headers.
pub(crate) fn env_badge(environment: Option<Environment>) -> Option<&'static str> {
    match environment {
        Some(Environment::Dev) => Some("DEV"),
        Some(Environment::Staging) => Some("STAGING"),
        Some(Environment::Prod) => Some("PROD"),
        Some(_) | None => None,
    }
}

impl Tab {
    /// The tab id.
    #[must_use]
    pub fn id(&self) -> TabId {
        self.id
    }

    /// The connection this tab is bound to.
    #[must_use]
    pub fn connection(&self) -> &TabConnection {
        &self.connection
    }

    /// The tab kind.
    #[must_use]
    pub fn kind(&self) -> TabKind {
        self.kind
    }

    /// The connection name, plus ` (n)` for the n-th tab opened for that name (n >= 2). Grid tabs
    /// are prefixed with `Results: `. A tab opened for a database object shows the object name
    /// instead: `DDL: <object>`, `Properties: <object>`, `Results: <object>`.
    #[must_use]
    pub fn title(&self) -> String {
        if let Some(object) = &self.object {
            return match self.kind {
                TabKind::Ddl => format!("DDL: {object}"),
                TabKind::Properties => format!("Properties: {object}"),
                TabKind::Grid => format!("Results: {object}"),
                TabKind::Query => object.clone(),
            };
        }
        let base = if self.ordinal >= 2 {
            format!("{} ({})", self.connection.name, self.ordinal)
        } else {
            self.connection.name.clone()
        };
        match self.kind {
            TabKind::Grid => format!("Results: {base}"),
            TabKind::Query | TabKind::Ddl | TabKind::Properties => base,
        }
    }

    /// The header data: title, accent colour and environment badge.
    #[must_use]
    pub fn header(&self) -> TabHeader {
        let badge = env_badge(self.connection.environment);
        TabHeader {
            title: self.title(),
            accent: self.connection.color.as_deref().and_then(Rgb::parse),
            badge,
            is_prod: self.connection.environment == Some(Environment::Prod),
        }
    }
}

/// The ordered tabs and the active one.
#[derive(Debug, Default)]
pub struct TabStrip {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: u64,
    opened_per_name: HashMap<String, u32>,
}

impl TabStrip {
    /// Opens a tab at the end and activates it.
    pub fn open(&mut self, connection: TabConnection, kind: TabKind) -> TabId {
        self.open_tab(connection, kind, None)
    }

    /// Opens a tab for a database object at the end and activates it; the title names the object.
    pub fn open_object(
        &mut self,
        connection: TabConnection,
        kind: TabKind,
        object: impl Into<String>,
    ) -> TabId {
        self.open_tab(connection, kind, Some(object.into()))
    }

    fn open_tab(
        &mut self,
        connection: TabConnection,
        kind: TabKind,
        object: Option<String>,
    ) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let counter = self
            .opened_per_name
            .entry(connection.name.clone())
            .or_insert(0);
        *counter += 1;
        self.tabs.push(Tab {
            id,
            connection,
            kind,
            ordinal: *counter,
            object,
        });
        self.active = Some(id);
        id
    }

    fn position(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Activates a tab; `false` if it does not exist.
    pub fn activate(&mut self, id: TabId) -> bool {
        if self.position(id).is_some() {
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    /// Closes a tab; `false` if it does not exist. Closing the active tab activates its right
    /// neighbour, otherwise the left one, otherwise none.
    pub fn close(&mut self, id: TabId) -> bool {
        let Some(pos) = self.position(id) else {
            return false;
        };
        self.tabs.remove(pos);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(pos)
                .or_else(|| pos.checked_sub(1).and_then(|p| self.tabs.get(p)))
                .map(|t| t.id);
        }
        true
    }

    fn step(&mut self, forward: bool) {
        let len = self.tabs.len();
        let Some(pos) = self.active.and_then(|id| self.position(id)) else {
            return;
        };
        let next = if forward {
            (pos + 1) % len
        } else {
            (pos + len - 1) % len
        };
        self.active = Some(self.tabs[next].id);
    }

    /// Activates the next tab, wrapping around.
    pub fn next(&mut self) {
        self.step(true);
    }

    /// Activates the previous tab, wrapping around.
    pub fn previous(&mut self) {
        self.step(false);
    }

    /// The active tab.
    #[must_use]
    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    /// A tab by id.
    #[must_use]
    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    /// The tabs in order.
    pub fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    /// The number of tabs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Whether there are no tabs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(name: &str) -> TabConnection {
        TabConnection {
            name: name.to_owned(),
            driver: DriverId::new("sqlite").unwrap(),
            color: None,
            environment: None,
        }
    }

    fn ids(s: &TabStrip) -> Vec<TabId> {
        s.iter().map(Tab::id).collect()
    }

    #[test]
    fn open_activates_new_tab() {
        let mut s = TabStrip::default();
        assert!(s.is_empty());
        assert!(s.active().is_none());
        let a = s.open(conn("a"), TabKind::Query);
        let b = s.open(conn("b"), TabKind::Query);
        assert_eq!(s.len(), 2);
        assert_eq!(s.active().map(Tab::id), Some(b));
        assert!(s.activate(a));
        assert_eq!(s.active().map(Tab::id), Some(a));
        assert_eq!(s.get(b).map(Tab::kind), Some(TabKind::Query));
    }

    #[test]
    fn object_tabs_are_titled_after_the_object() {
        let mut s = TabStrip::default();
        let ddl = s.open_object(conn("a"), TabKind::Ddl, "\"main\".\"users\"");
        let props = s.open_object(conn("a"), TabKind::Properties, "users");
        let data = s.open_object(conn("a"), TabKind::Grid, "users");
        assert_eq!(s.get(ddl).unwrap().title(), "DDL: \"main\".\"users\"");
        assert_eq!(s.get(props).unwrap().title(), "Properties: users");
        assert_eq!(s.get(data).unwrap().title(), "Results: users");
        assert_eq!(s.get(ddl).map(Tab::kind), Some(TabKind::Ddl));
        // The header carries the connection's colour and badge, as for every tab.
        assert_eq!(s.get(props).unwrap().header().title, "Properties: users");
    }

    #[test]
    fn activate_unknown_is_false() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        s.close(a);
        assert!(!s.activate(a));
        assert!(!s.close(a));
    }

    #[test]
    fn close_active_prefers_right_then_left_then_none() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        let b = s.open(conn("b"), TabKind::Query);
        let c = s.open(conn("c"), TabKind::Query);
        s.activate(b);
        assert!(s.close(b));
        assert_eq!(s.active().map(Tab::id), Some(c));
        assert!(s.close(c));
        assert_eq!(s.active().map(Tab::id), Some(a));
        assert!(s.close(a));
        assert!(s.active().is_none());
        assert!(s.is_empty());
    }

    #[test]
    fn close_inactive_keeps_active() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        let b = s.open(conn("b"), TabKind::Query);
        assert!(s.close(a));
        assert_eq!(s.active().map(Tab::id), Some(b));
    }

    #[test]
    fn next_and_previous_wrap() {
        let mut s = TabStrip::default();
        s.next();
        s.previous();
        let a = s.open(conn("a"), TabKind::Query);
        let b = s.open(conn("b"), TabKind::Query);
        let c = s.open(conn("c"), TabKind::Query);
        s.next();
        assert_eq!(s.active().map(Tab::id), Some(a));
        s.previous();
        assert_eq!(s.active().map(Tab::id), Some(c));
        s.previous();
        assert_eq!(s.active().map(Tab::id), Some(b));
        assert_eq!(ids(&s), [a, b, c]);
    }

    #[test]
    fn several_tabs_per_connection_get_numbered_titles() {
        let mut s = TabStrip::default();
        let t1 = s.open(conn("x"), TabKind::Query);
        let t2 = s.open(conn("x"), TabKind::Query);
        let t3 = s.open(conn("x"), TabKind::Query);
        s.close(t2);
        let t4 = s.open(conn("x"), TabKind::Query);
        let title = |id| s.get(id).unwrap().title();
        assert_eq!(title(t1), "x");
        assert_eq!(title(t3), "x (3)");
        assert_eq!(title(t4), "x (4)");
        assert!(t1 < t3 && t3 < t4);
    }

    #[test]
    fn header_badge_and_prod() {
        let mut s = TabStrip::default();
        let cases = [
            (None, None, false),
            (Some(Environment::Dev), Some("DEV"), false),
            (Some(Environment::Staging), Some("STAGING"), false),
            (Some(Environment::Prod), Some("PROD"), true),
        ];
        for (env, badge, prod) in cases {
            let mut c = conn("n");
            c.environment = env;
            c.color = Some("#102030".to_owned());
            let id = s.open(c, TabKind::Query);
            let h = s.get(id).unwrap().header();
            assert_eq!(h.badge, badge);
            assert_eq!(h.is_prod, prod);
            assert_eq!(h.accent, Some(Rgb(0x0010_2030)));
        }
    }

    #[test]
    fn header_without_valid_colour_has_no_accent() {
        let mut s = TabStrip::default();
        let mut c = conn("n");
        c.color = Some("red".to_owned());
        let id = s.open(c, TabKind::Query);
        assert_eq!(s.get(id).unwrap().header().accent, None);
        assert_eq!(s.get(id).unwrap().connection().name, "n");
    }

    #[test]
    fn grid_tab_title_is_prefixed_and_keeps_the_ordinal() {
        let mut s = TabStrip::default();
        let q = s.open(conn("x"), TabKind::Query);
        let g = s.open(conn("x"), TabKind::Grid);
        assert_eq!(s.get(q).unwrap().title(), "x");
        assert_eq!(s.get(g).unwrap().title(), "Results: x (2)");
        assert_eq!(s.get(g).unwrap().kind(), TabKind::Grid);
        assert_eq!(s.get(g).unwrap().header().title, "Results: x (2)");
        let first = s.open(conn("y"), TabKind::Grid);
        assert_eq!(s.get(first).unwrap().title(), "Results: y");
    }
}
