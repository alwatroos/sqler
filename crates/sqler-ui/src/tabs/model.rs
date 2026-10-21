// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `TabStrip`: ordered tabs, each bound to one connection.

use std::collections::HashMap;

use sqler_driver_api::sqler_core::{DriverId, Environment};

use super::header::{Rgb, TabHeader};

/// The most editor groups a strip holds.
pub const MAX_GROUPS: usize = 4;

/// Stable identifier of an editor group; never reused within one strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(u32);

/// Whether a tab is a replaceable preview or kept (pinned).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabMode {
    /// Replaced by the next preview opened in the same group.
    Preview,
    /// Stays until closed.
    Pinned,
}

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
    /// The query history of one connection, with a search box.
    History,
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
    mode: TabMode,
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

    /// Whether the tab is a preview or pinned.
    #[must_use]
    pub fn mode(&self) -> TabMode {
        self.mode
    }

    /// The connection name, plus ` (n)` for the n-th tab opened for that name (n >= 2). Grid tabs
    /// are prefixed with `Results: `. A tab opened for a database object shows the object name
    /// instead: `DDL: <object>`, `Properties: <object>`, `Results: <object>`, `History: <object>`.
    #[must_use]
    pub fn title(&self) -> String {
        if let Some(object) = &self.object {
            return match self.kind {
                TabKind::Ddl => format!("DDL: {object}"),
                TabKind::Properties => format!("Properties: {object}"),
                TabKind::Grid => format!("Results: {object}"),
                TabKind::History => format!("History: {object}"),
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
            TabKind::History => format!("History: {base}"),
            TabKind::Query | TabKind::Ddl | TabKind::Properties => base,
        }
    }

    /// The header data: title, accent colour, environment badge, kind and preview state.
    #[must_use]
    pub fn header(&self) -> TabHeader {
        let badge = env_badge(self.connection.environment);
        TabHeader {
            title: self.title(),
            accent: self.connection.color.as_deref().and_then(Rgb::parse),
            badge,
            is_prod: self.connection.environment == Some(Environment::Prod),
            kind: self.kind,
            preview: self.mode == TabMode::Preview,
        }
    }
}

/// One editor group: an ordered list of tabs and its own active tab.
#[derive(Debug, Clone)]
pub struct Group {
    id: GroupId,
    tabs: Vec<TabId>,
    active: Option<TabId>,
}

impl Group {
    fn new(id: GroupId) -> Self {
        Self {
            id,
            tabs: Vec::new(),
            active: None,
        }
    }

    /// The group id.
    #[must_use]
    pub fn id(&self) -> GroupId {
        self.id
    }

    /// The tab ids in order.
    #[must_use]
    pub fn tabs(&self) -> &[TabId] {
        &self.tabs
    }

    /// The group's active tab.
    #[must_use]
    pub fn active(&self) -> Option<TabId> {
        self.active
    }

    /// Removes a tab; an active one hands over to its right, then left neighbour.
    fn remove(&mut self, id: TabId) {
        let Some(pos) = self.tabs.iter().position(|t| *t == id) else {
            return;
        };
        self.tabs.remove(pos);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(pos)
                .or_else(|| pos.checked_sub(1).and_then(|p| self.tabs.get(p)))
                .copied();
        }
    }
}

/// The ordered tabs, split into editor groups, and the active one.
#[derive(Debug)]
pub struct TabStrip {
    tabs: Vec<Tab>,
    groups: Vec<Group>,
    focused: GroupId,
    next_group: u32,
    next_id: u64,
    opened_per_name: HashMap<String, u32>,
}

impl Default for TabStrip {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            groups: vec![Group::new(GroupId(0))],
            focused: GroupId(0),
            next_group: 1,
            next_id: 0,
            opened_per_name: HashMap::new(),
        }
    }
}

impl TabStrip {
    /// Opens a pinned tab at the end of the focused group and activates it.
    pub fn open(&mut self, connection: TabConnection, kind: TabKind) -> TabId {
        self.open_tab(connection, kind, None, TabMode::Pinned, None)
    }

    /// Opens a pinned tab for a database object at the end of the focused group and activates
    /// it; the title names the object.
    pub fn open_object(
        &mut self,
        connection: TabConnection,
        kind: TabKind,
        object: impl Into<String>,
    ) -> TabId {
        self.open_tab(connection, kind, Some(object.into()), TabMode::Pinned, None)
    }

    /// Opens a preview tab for a database object in the focused group at `at` (clamped; `None`
    /// is the end) and activates it. An older preview is left to the caller to close.
    pub fn open_preview(
        &mut self,
        connection: TabConnection,
        kind: TabKind,
        object: impl Into<String>,
        at: Option<usize>,
    ) -> TabId {
        self.open_tab(connection, kind, Some(object.into()), TabMode::Preview, at)
    }

    fn open_tab(
        &mut self,
        connection: TabConnection,
        kind: TabKind,
        object: Option<String>,
        mode: TabMode,
        at: Option<usize>,
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
            mode,
        });
        let gi = self.focused_index();
        let group = &mut self.groups[gi];
        let index = at.map_or(group.tabs.len(), |i| i.min(group.tabs.len()));
        group.tabs.insert(index, id);
        group.active = Some(id);
        id
    }

    fn focused_index(&self) -> usize {
        self.group_index(self.focused).unwrap_or(0)
    }

    fn group_index(&self, g: GroupId) -> Option<usize> {
        self.groups.iter().position(|x| x.id == g)
    }

    fn group_index_of(&self, id: TabId) -> Option<usize> {
        self.groups.iter().position(|g| g.tabs.contains(&id))
    }

    /// Removes group `gi` if it is empty and not the last one; moves focus left if it was focused.
    fn drop_if_empty(&mut self, gi: usize) {
        if self.groups[gi].tabs.is_empty() && self.groups.len() > 1 {
            let removed = self.groups.remove(gi).id;
            if self.focused == removed {
                self.focused = self.groups[gi.saturating_sub(1)].id;
            }
        }
    }

    /// Activates a tab and focuses its group; `false` if it does not exist.
    pub fn activate(&mut self, id: TabId) -> bool {
        let Some(gi) = self.group_index_of(id) else {
            return false;
        };
        self.groups[gi].active = Some(id);
        self.focused = self.groups[gi].id;
        true
    }

    /// Closes a tab; `false` if it does not exist. Closing a group's active tab activates its
    /// right neighbour in the group, otherwise the left one, otherwise none. An emptied group is
    /// removed unless it is the last; focus then moves to the group on the left.
    pub fn close(&mut self, id: TabId) -> bool {
        let Some(gi) = self.group_index_of(id) else {
            return false;
        };
        self.groups[gi].remove(id);
        self.tabs.retain(|t| t.id != id);
        self.drop_if_empty(gi);
        true
    }

    fn step(&mut self, forward: bool) {
        let gi = self.focused_index();
        let group = &mut self.groups[gi];
        let len = group.tabs.len();
        let Some(pos) = group
            .active
            .and_then(|id| group.tabs.iter().position(|t| *t == id))
        else {
            return;
        };
        let next = if forward {
            (pos + 1) % len
        } else {
            (pos + len - 1) % len
        };
        group.active = Some(group.tabs[next]);
    }

    /// Activates the next tab of the focused group, wrapping around.
    pub fn next(&mut self) {
        self.step(true);
    }

    /// Activates the previous tab of the focused group, wrapping around.
    pub fn previous(&mut self) {
        self.step(false);
    }

    /// The active tab of the focused group.
    #[must_use]
    pub fn active(&self) -> Option<&Tab> {
        self.active_in(self.focused)
    }

    /// A tab by id.
    #[must_use]
    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    /// The tabs: group by group, in group order.
    pub fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.groups
            .iter()
            .flat_map(|g| g.tabs.iter())
            .filter_map(|id| self.get(*id))
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

    /// The editor groups, left to right; never empty.
    #[must_use]
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// The focused group.
    #[must_use]
    pub fn focused_group(&self) -> GroupId {
        self.focused
    }

    /// Focuses a group; `false` if it does not exist or is already focused.
    pub fn focus_group(&mut self, g: GroupId) -> bool {
        if self.focused == g || self.group_index(g).is_none() {
            return false;
        }
        self.focused = g;
        true
    }

    /// Focuses the next or previous group without wrapping; `false` at the edge.
    pub fn focus_adjacent_group(&mut self, forward: bool) -> bool {
        match self.adjacent_group(forward) {
            Some(g) => self.focus_group(g),
            None => false,
        }
    }

    fn adjacent_group(&self, forward: bool) -> Option<GroupId> {
        let gi = self.focused_index();
        let target = if forward { gi + 1 } else { gi.checked_sub(1)? };
        self.groups.get(target).map(Group::id)
    }

    /// The group a tab is in.
    #[must_use]
    pub fn group_of(&self, id: TabId) -> Option<GroupId> {
        self.group_index_of(id).map(|gi| self.groups[gi].id)
    }

    /// The index of a tab inside its group.
    #[must_use]
    pub fn position_in_group(&self, id: TabId) -> Option<usize> {
        self.groups
            .iter()
            .find_map(|g| g.tabs.iter().position(|t| *t == id))
    }

    /// The active tab of a group.
    #[must_use]
    pub fn active_in(&self, g: GroupId) -> Option<&Tab> {
        let group = self.groups.get(self.group_index(g)?)?;
        group.active.and_then(|id| self.get(id))
    }

    /// The preview tab of a group, if any.
    #[must_use]
    pub fn preview_in(&self, g: GroupId) -> Option<TabId> {
        let group = self.groups.get(self.group_index(g)?)?;
        group
            .tabs
            .iter()
            .copied()
            .find(|id| self.mode(*id) == Some(TabMode::Preview))
    }

    /// Whether a tab is a preview or pinned.
    #[must_use]
    pub fn mode(&self, id: TabId) -> Option<TabMode> {
        self.get(id).map(Tab::mode)
    }

    /// Pins a preview tab; `true` iff it changed.
    pub fn pin(&mut self, id: TabId) -> bool {
        match self.tabs.iter_mut().find(|t| t.id == id) {
            Some(tab) if tab.mode == TabMode::Preview => {
                tab.mode = TabMode::Pinned;
                true
            }
            _ => false,
        }
    }

    /// Moves a tab to `index` of group `to` (clamped after removal), pins it, activates it there
    /// and focuses `to`. Within one group this reorders. An emptied source group is removed.
    pub fn move_tab(&mut self, id: TabId, to: GroupId, index: usize) -> bool {
        let (Some(from), Some(dest)) = (self.group_index_of(id), self.group_index(to)) else {
            return false;
        };
        self.groups[from].remove(id);
        let group = &mut self.groups[dest];
        group.tabs.insert(index.min(group.tabs.len()), id);
        group.active = Some(id);
        self.focused = to;
        if from != dest {
            self.drop_if_empty(from);
        }
        self.pin(id);
        true
    }

    /// Moves the active tab one place within its group, without wrapping; `false` at an edge.
    pub fn move_active_by(&mut self, delta: isize) -> bool {
        let gi = self.focused_index();
        let group = &mut self.groups[gi];
        let Some(pos) = group
            .active
            .and_then(|id| group.tabs.iter().position(|t| *t == id))
        else {
            return false;
        };
        let Some(target) = pos.checked_add_signed(delta) else {
            return false;
        };
        if delta == 0 || target >= group.tabs.len() {
            return false;
        }
        let id = group.tabs.remove(pos);
        group.tabs.insert(target, id);
        true
    }

    /// Moves the active tab to the end of the next or previous group; `false` without one.
    pub fn move_active_to_adjacent_group(&mut self, forward: bool) -> bool {
        let Some(id) = self.active().map(Tab::id) else {
            return false;
        };
        let Some(to) = self.adjacent_group(forward) else {
            return false;
        };
        self.move_tab(id, to, usize::MAX)
    }

    /// Opens a new group right of the focused one and moves the active tab into it. `None` when
    /// the focused group has fewer than two tabs or there are already [`MAX_GROUPS`] groups.
    pub fn split_right(&mut self) -> Option<GroupId> {
        let gi = self.focused_index();
        if self.groups[gi].tabs.len() < 2 || self.groups.len() >= MAX_GROUPS {
            return None;
        }
        let id = self.groups[gi].active?;
        let new = GroupId(self.next_group);
        self.next_group += 1;
        self.groups.insert(gi + 1, Group::new(new));
        self.move_tab(id, new, 0).then_some(new)
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

    #[test]
    fn history_tab_title_names_the_object_or_the_connection() {
        let mut s = TabStrip::default();
        let named = s.open_object(conn("x"), TabKind::History, "prod");
        let plain = s.open(conn("y"), TabKind::History);
        assert_eq!(s.get(named).unwrap().title(), "History: prod");
        assert_eq!(s.get(plain).unwrap().title(), "History: y");
    }

    fn titles(s: &TabStrip, g: GroupId) -> Vec<TabId> {
        s.groups()
            .iter()
            .find(|x| x.id() == g)
            .unwrap()
            .tabs()
            .to_vec()
    }

    #[test]
    fn default_has_one_empty_group() {
        let mut s = TabStrip::default();
        assert_eq!(s.groups().len(), 1);
        assert_eq!(s.focused_group(), s.groups()[0].id());
        assert!(!s.focus_group(s.focused_group()));
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn preview_open_pin_and_header_flags() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        let c = s.open(conn("a"), TabKind::Query);
        let p = s.open_preview(conn("a"), TabKind::Grid, "t", Some(1));
        let grp = s.focused_group();
        assert_eq!(titles(&s, grp), [a, p, c]);
        assert_eq!(s.active().map(Tab::id), Some(p));
        assert_eq!(s.preview_in(grp), Some(p));
        assert_eq!(s.mode(p), Some(TabMode::Preview));
        let h = s.get(p).unwrap().header();
        assert!(h.preview);
        assert_eq!(h.kind, TabKind::Grid);
        assert!(!s.get(a).unwrap().header().preview);
        assert!(s.pin(p));
        assert!(!s.pin(p));
        assert_eq!(s.preview_in(grp), None);
        let q = s.open_preview(conn("a"), TabKind::Grid, "u", Some(99));
        assert_eq!(s.position_in_group(q), Some(3));
    }

    #[test]
    fn split_refusals_and_effect() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        assert_eq!(s.split_right(), None);
        let b = s.open(conn("b"), TabKind::Query);
        let g2 = s.split_right().unwrap();
        assert_eq!(s.focused_group(), g2);
        assert_eq!(titles(&s, g2), [b]);
        assert_eq!(s.active().map(Tab::id), Some(b));
        assert_eq!(s.group_of(a), Some(s.groups()[0].id()));
        // one tab in the focused group: refused
        assert_eq!(s.split_right(), None);
    }

    #[test]
    fn split_stops_at_max_groups() {
        let mut s = TabStrip::default();
        for _ in 0..8 {
            s.open(conn("a"), TabKind::Query);
        }
        for _ in 0..MAX_GROUPS - 1 {
            assert!(s.split_right().is_some());
            s.open(conn("a"), TabKind::Query);
        }
        assert_eq!(s.groups().len(), MAX_GROUPS);
        assert_eq!(s.split_right(), None);
    }

    #[test]
    fn closing_last_tab_removes_group_and_focuses_left() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        let b = s.open(conn("b"), TabKind::Query);
        let g1 = s.focused_group();
        let g2 = s.split_right().unwrap();
        assert!(s.close(b));
        assert_eq!(s.groups().len(), 1);
        assert_eq!(s.focused_group(), g1);
        assert_eq!(s.active().map(Tab::id), Some(a));
        assert!(s.group_of(b).is_none());
        let _ = g2;
        // the last group stays when emptied
        assert!(s.close(a));
        assert_eq!(s.groups().len(), 1);
        assert!(s.active().is_none());
    }

    #[test]
    fn activate_focuses_group_and_iter_is_group_order() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        let b = s.open(conn("b"), TabKind::Query);
        let c = s.open(conn("c"), TabKind::Query);
        let g1 = s.focused_group();
        let g2 = s.split_right().unwrap(); // c moves right
        assert_eq!(ids(&s), [a, b, c]);
        assert_eq!(s.active().map(Tab::id), Some(c));
        assert!(s.activate(b));
        assert_eq!(s.focused_group(), g1);
        assert_eq!(s.active_in(g2).map(Tab::id), Some(c));
        s.next();
        assert_eq!(s.active().map(Tab::id), Some(a));
    }

    #[test]
    fn move_tab_between_groups_pins_and_removes_empty_source() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        let p = s.open_preview(conn("a"), TabKind::Grid, "t", None);
        let g1 = s.focused_group();
        let g2 = s.split_right().unwrap();
        assert_eq!(s.mode(p), Some(TabMode::Pinned));
        assert!(s.move_tab(a, g2, 0));
        assert_eq!(s.groups().len(), 1);
        assert_eq!(s.focused_group(), g2);
        assert_eq!(titles(&s, g2), [a, p]);
        assert!(!s.move_tab(a, g1, 0));
    }

    #[test]
    fn move_active_by_reorders_without_wrapping() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        let b = s.open(conn("b"), TabKind::Query);
        assert!(!s.move_active_by(1));
        assert!(s.move_active_by(-1));
        assert_eq!(ids(&s), [b, a]);
        assert!(!s.move_active_by(-1));
        assert!(!TabStrip::default().move_active_by(1));
    }

    #[test]
    fn adjacent_group_focus_and_move() {
        let mut s = TabStrip::default();
        let a = s.open(conn("a"), TabKind::Query);
        let b = s.open(conn("b"), TabKind::Query);
        assert!(!s.move_active_to_adjacent_group(true));
        assert!(!s.focus_adjacent_group(true));
        let g1 = s.focused_group();
        let g2 = s.split_right().unwrap();
        assert!(!s.focus_adjacent_group(true));
        assert!(s.focus_adjacent_group(false));
        assert_eq!(s.focused_group(), g1);
        assert!(s.focus_group(g2));
        assert!(s.move_active_to_adjacent_group(false));
        assert_eq!(titles(&s, g1), [a, b]);
        assert_eq!(s.groups().len(), 1);
    }
}
