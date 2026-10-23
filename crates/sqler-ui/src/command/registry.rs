// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Command` and `CommandRegistry`.

use std::collections::BTreeMap;

use super::error::CommandError;
use super::id::CommandId;

/// A user action with a stable ID and a human title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    id: CommandId,
    title: String,
}

impl Command {
    /// Builds a command, validating the ID.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::InvalidId`] if `id` does not match `category:action`.
    pub fn new(id: &str, title: impl Into<String>) -> Result<Self, CommandError> {
        Ok(Self {
            id: CommandId::new(id)?,
            title: title.into(),
        })
    }

    /// The command ID.
    #[must_use]
    pub fn id(&self) -> &CommandId {
        &self.id
    }

    /// The human-readable title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The category: the ID prefix.
    #[must_use]
    pub fn category(&self) -> &str {
        self.id.category()
    }
}

/// The single place where commands are registered. Iteration is sorted by ID.
#[derive(Debug, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<CommandId, Command>,
}

impl CommandRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a command.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::Duplicate`] if the ID is already registered; the registry is
    /// left unchanged.
    pub fn register(&mut self, command: Command) -> Result<(), CommandError> {
        if self.commands.contains_key(command.id()) {
            return Err(CommandError::Duplicate(command.id().as_str().to_owned()));
        }
        self.commands.insert(command.id().clone(), command);
        Ok(())
    }

    /// Looks a command up by ID.
    #[must_use]
    pub fn get(&self, id: &CommandId) -> Option<&Command> {
        self.commands.get(id)
    }

    /// Iterates over all commands, sorted by ID.
    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.values()
    }

    /// Keeps only the commands for which `keep` is true. The plugin commands are dropped and registered again after
    /// every plugin load this way.
    pub fn retain(&mut self, mut keep: impl FnMut(&Command) -> bool) {
        self.commands.retain(|_, command| keep(command));
    }

    /// The number of registered commands.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether the registry has no commands.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(id: &str, title: &str) -> Command {
        Command::new(id, title).unwrap()
    }

    #[test]
    fn register_and_get() {
        let mut r = CommandRegistry::new();
        assert!(r.is_empty());
        r.register(cmd("query:run-all", "Run All")).unwrap();
        let id = CommandId::new("query:run-all").unwrap();
        assert_eq!(r.get(&id).unwrap().title(), "Run All");
        assert_eq!(r.len(), 1);
        assert!(r.get(&CommandId::new("query:run-none").unwrap()).is_none());
    }

    #[test]
    fn duplicate_rejected() {
        let mut r = CommandRegistry::new();
        r.register(cmd("query:run-all", "Run All")).unwrap();
        let err = r.register(cmd("query:run-all", "Other")).unwrap_err();
        assert_eq!(err, CommandError::Duplicate("query:run-all".into()));
        assert_eq!(r.len(), 1);
        let id = CommandId::new("query:run-all").unwrap();
        assert_eq!(r.get(&id).unwrap().title(), "Run All");
    }

    #[test]
    fn bad_id_rejected() {
        assert_eq!(
            Command::new("nope", "x"),
            Err(CommandError::InvalidId("nope".into()))
        );
    }

    #[test]
    fn iter_sorted_by_id() {
        let mut r = CommandRegistry::new();
        r.register(cmd("query:run-all", "Run All")).unwrap();
        r.register(cmd("app:open-settings", "Open Settings"))
            .unwrap();
        let ids: Vec<_> = r.iter().map(|c| c.id().as_str()).collect();
        assert_eq!(ids, ["app:open-settings", "query:run-all"]);
    }

    #[test]
    fn retain_drops_what_does_not_match() {
        let mut r = CommandRegistry::new();
        r.register(cmd("query:run-all", "Run All")).unwrap();
        r.register(cmd("plugin-acme:run", "Run")).unwrap();
        r.register(cmd("panel-plugin-acme:main", "Show")).unwrap();
        r.retain(|c| !c.category().contains("plugin"));
        assert_eq!(r.len(), 1);
        assert!(r.get(&CommandId::new("query:run-all").unwrap()).is_some());
        // The ids are free again.
        r.register(cmd("plugin-acme:run", "Run")).unwrap();
    }

    #[test]
    fn category_is_prefix() {
        assert_eq!(
            cmd("result-grid:copy-as-csv", "Copy").category(),
            "result-grid"
        );
    }
}
