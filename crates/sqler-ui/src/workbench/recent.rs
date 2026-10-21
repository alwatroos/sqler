// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Most-recently-used lists of commands and connections.

use sqler_config::{ConnectionId, MAX_RECENT_COMMANDS, MAX_RECENT_CONNECTIONS};

use crate::command::{CommandId, CommandRegistry};
use crate::palette::NEEDS_ARGUMENT as ARGUMENT_COMMANDS;
use crate::sidebar::SidebarModel;

/// The recently run commands, most recent first, at most [`MAX_RECENT_COMMANDS`].
#[derive(Debug, Clone, Default)]
pub struct RecentCommands {
    ids: Vec<CommandId>,
}

impl RecentCommands {
    /// Moves `id` to the front, inserting it when new, and drops the oldest over the cap.
    pub fn record(&mut self, id: &CommandId) {
        self.ids.retain(|i| i != id);
        self.ids.insert(0, id.clone());
        self.ids.truncate(MAX_RECENT_COMMANDS);
    }

    /// The IDs, most recent first.
    pub fn iter(&self) -> impl Iterator<Item = &CommandId> {
        self.ids.iter()
    }

    /// Restores a stored list: invalid, unregistered and argument-taking IDs and duplicates
    /// are dropped, and the list is capped.
    #[must_use]
    pub fn from_strings(s: &[String], r: &CommandRegistry) -> Self {
        let mut ids: Vec<CommandId> = Vec::new();
        for text in s {
            if ARGUMENT_COMMANDS.contains(&text.as_str()) {
                continue;
            }
            let Ok(id) = CommandId::new(text) else {
                continue;
            };
            if r.get(&id).is_some() && !ids.contains(&id) {
                ids.push(id);
            }
            if ids.len() == MAX_RECENT_COMMANDS {
                break;
            }
        }
        Self { ids }
    }

    /// The IDs as stored text, most recent first.
    #[must_use]
    pub fn to_strings(&self) -> Vec<String> {
        self.ids.iter().map(|i| i.as_str().to_owned()).collect()
    }
}

/// The recently opened connections, most recent first, at most [`MAX_RECENT_CONNECTIONS`].
#[derive(Debug, Clone, Default)]
pub struct RecentConnections {
    ids: Vec<ConnectionId>,
}

impl RecentConnections {
    /// Moves `id` to the front, inserting it when new, and drops the oldest over the cap.
    pub fn record(&mut self, id: &ConnectionId) {
        self.ids.retain(|i| i != id);
        self.ids.insert(0, id.clone());
        self.ids.truncate(MAX_RECENT_CONNECTIONS);
    }

    /// The IDs, most recent first.
    pub fn iter(&self) -> impl Iterator<Item = &ConnectionId> {
        self.ids.iter()
    }

    /// Restores a stored list, keeping only connections that exist in `sidebar`.
    #[must_use]
    pub fn from_strings(s: &[String], sidebar: &SidebarModel) -> Self {
        let mut ids: Vec<ConnectionId> = Vec::new();
        for text in s {
            let found = sidebar.entries().iter().find(|e| e.id().as_str() == text);
            if let Some(entry) = found
                && !ids.contains(entry.id())
            {
                ids.push(entry.id().clone());
            }
            if ids.len() == MAX_RECENT_CONNECTIONS {
                break;
            }
        }
        Self { ids }
    }

    /// The IDs as stored text, most recent first.
    #[must_use]
    pub fn to_strings(&self) -> Vec<String> {
        self.ids.iter().map(|i| i.as_str().to_owned()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;

    fn id(s: &str) -> CommandId {
        CommandId::new(s).unwrap()
    }

    fn registry(ids: &[&str]) -> CommandRegistry {
        let mut r = CommandRegistry::new();
        for i in ids {
            r.register(Command::new(i, *i).unwrap()).unwrap();
        }
        r
    }

    #[test]
    fn record_is_mru_and_capped() {
        let mut r = RecentCommands::default();
        for i in 0..12 {
            r.record(&id(&format!("tab:cmd-{}", char::from(b'a' + i))));
        }
        assert_eq!(r.iter().count(), 10);
        assert_eq!(r.iter().next().unwrap().as_str(), "tab:cmd-l");
        r.record(&id("tab:cmd-h"));
        let v = r.to_strings();
        assert_eq!(v[0], "tab:cmd-h");
        assert_eq!(v.iter().filter(|s| *s == "tab:cmd-h").count(), 1);
        assert_eq!(v.len(), 10);
    }

    #[test]
    fn from_strings_drops_bad_entries() {
        let reg = registry(&["tab:close", "app:quit", "tab:move", "pane:focus"]);
        let s: Vec<String> = [
            "tab:close",
            "nope",
            "gone:cmd",
            "tab:move",
            "app:quit",
            "tab:close",
            "pane:focus",
        ]
        .map(String::from)
        .into();
        let r = RecentCommands::from_strings(&s, &reg);
        assert_eq!(r.to_strings(), ["tab:close", "app:quit"]);
    }

    #[test]
    fn from_strings_caps() {
        let names: Vec<String> = (0..15)
            .map(|i| format!("tab:cmd-{}", char::from(b'a' + i)))
            .collect();
        let reg = registry(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let r = RecentCommands::from_strings(&names, &reg);
        assert_eq!(r.to_strings().len(), 10);
        assert_eq!(r.to_strings()[0], "tab:cmd-a");
    }

    fn sidebar(names: &[&str]) -> SidebarModel {
        use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};
        SidebarModel::from_connections(
            names
                .iter()
                .map(|n| ConnectionConfig::new(*n, DriverId::new("sqlite").unwrap(), ":memory:"))
                .collect(),
        )
    }

    #[test]
    fn connections_record_mru_and_cap() {
        let sb = sidebar(&["a", "b", "c", "d", "e", "f"]);
        let ids: Vec<ConnectionId> = sb.entries().iter().map(|e| e.id().clone()).collect();
        let mut r = RecentConnections::default();
        for i in &ids {
            r.record(i);
        }
        assert_eq!(r.iter().count(), 5);
        assert_eq!(r.iter().next(), Some(&ids[5]));
        r.record(&ids[2]);
        assert_eq!(r.to_strings(), ["c", "f", "e", "d", "b"]);
    }

    #[test]
    fn connections_from_strings_keeps_existing_only() {
        let sb = sidebar(&["a", "b", "c"]);
        let s: Vec<String> = ["c", "gone", "a", "c"].map(String::from).into();
        let r = RecentConnections::from_strings(&s, &sb);
        assert_eq!(r.to_strings(), ["c", "a"]);
    }
}
