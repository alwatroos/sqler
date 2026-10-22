// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `AiAction`: every action of the AI settings and the AI view, each mapped to a command ID.
//! Later AI features add their rows here (ADR-0050).

use sqler_config::{ModelChoice, ProviderId, ProviderKind, UseCase};

use super::audit::AuditFilterChange;
use super::settings::AiField;
use crate::command::{Command, CommandError, CommandId, CommandRegistry};
use crate::workspace::Workspace;

/// A user action on the AI settings or the AI side view.
#[derive(Clone, PartialEq, Eq)]
pub enum AiAction {
    /// Open the settings page (the AI section is on it).
    OpenSettings,
    /// Open the editor for a new provider of this kind.
    AddProvider(ProviderKind),
    /// Open the editor for a saved provider.
    EditProvider(ProviderId),
    /// Remove a provider, its model choices and its key.
    RemoveProvider(ProviderId),
    /// Set a text field of the editor or the limits form.
    SetText {
        /// The field.
        field: AiField,
        /// The new text.
        text: String,
    },
    /// Choose the kind of the new provider in the editor.
    SetKind(ProviderKind),
    /// Remove the stored key when the provider is saved.
    ClearKey,
    /// Validate and save the editor's provider.
    SaveProvider,
    /// Close the editor without saving.
    CancelProvider,
    /// Test the editor's provider, else the provider of the connection.
    TestProvider,
    /// Put the models the test listed into the editor.
    UseListedModels,
    /// Choose the model of a use case (`None` = the default); `None` choice clears it.
    SetModel {
        /// The use case.
        use_case: Option<UseCase>,
        /// The provider and model.
        choice: Option<ModelChoice>,
    },
    /// Lift or restore the query caps in the limits form.
    SetUnlimited(bool),
    /// Validate and save the redaction list and the limits.
    SaveLimits,
    /// Turn AI on or off for the connection the user works on.
    ToggleConnection,
    /// Show the audit log view and read its first page.
    OpenAuditLog,
    /// Read the next page of the audit log.
    LoadMoreAudit,
    /// Copy the SQL of the selected audit entry.
    CopyAuditSql,
    /// Change one audit filter.
    SetAuditFilter(AuditFilterChange),
    /// Select a row of the audit log.
    SelectAuditEntry(usize),
}

// A typed API key travels in `SetText`; its `Debug` must not show it.
impl std::fmt::Debug for AiAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenSettings => f.write_str("OpenSettings"),
            Self::AddProvider(k) => f.debug_tuple("AddProvider").field(k).finish(),
            Self::EditProvider(id) => f.debug_tuple("EditProvider").field(id).finish(),
            Self::RemoveProvider(id) => f.debug_tuple("RemoveProvider").field(id).finish(),
            Self::SetText {
                field: AiField::Key,
                ..
            } => f.write_str("SetText { field: Key, text: *** }"),
            Self::SetText { field, text } => f
                .debug_struct("SetText")
                .field("field", field)
                .field("text", text)
                .finish(),
            Self::SetKind(k) => f.debug_tuple("SetKind").field(k).finish(),
            Self::ClearKey => f.write_str("ClearKey"),
            Self::SaveProvider => f.write_str("SaveProvider"),
            Self::CancelProvider => f.write_str("CancelProvider"),
            Self::TestProvider => f.write_str("TestProvider"),
            Self::UseListedModels => f.write_str("UseListedModels"),
            Self::SetModel { use_case, choice } => f
                .debug_struct("SetModel")
                .field("use_case", use_case)
                .field("choice", choice)
                .finish(),
            Self::SetUnlimited(on) => f.debug_tuple("SetUnlimited").field(on).finish(),
            Self::SaveLimits => f.write_str("SaveLimits"),
            Self::ToggleConnection => f.write_str("ToggleConnection"),
            Self::OpenAuditLog => f.write_str("OpenAuditLog"),
            Self::LoadMoreAudit => f.write_str("LoadMoreAudit"),
            Self::CopyAuditSql => f.write_str("CopyAuditSql"),
            Self::SetAuditFilter(change) => f.debug_tuple("SetAuditFilter").field(change).finish(),
            Self::SelectAuditEntry(ix) => f.debug_tuple("SelectAuditEntry").field(ix).finish(),
        }
    }
}

/// The commands: ID and title. None has a default chord.
pub const COMMANDS: &[(&str, &str)] = &[
    ("ai:open-settings", "AI: Open Settings"),
    ("ai:add-provider", "AI: Add Provider"),
    ("ai:test-provider", "AI: Test Provider"),
    ("ai:save-provider", "AI: Save Provider"),
    ("ai:cancel-provider", "AI: Close Provider Editor"),
    ("ai:edit-settings", "AI: Edit Settings"),
    (
        "ai:toggle-connection",
        "AI: Turn On or Off for This Connection",
    ),
    ("ai:open-audit-log", "AI: Open Audit Log"),
    ("ai:load-more-audit", "AI: Load More Audit Entries"),
    ("ai:copy-audit-sql", "AI: Copy SQL of Audit Entry"),
];

impl AiAction {
    /// The ID of the command this action runs. Field edits belong to `ai:edit-settings`.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(&self) -> CommandId {
        let id = match self {
            Self::OpenSettings => "ai:open-settings",
            Self::AddProvider(_) => "ai:add-provider",
            Self::TestProvider => "ai:test-provider",
            Self::SaveProvider => "ai:save-provider",
            Self::CancelProvider => "ai:cancel-provider",
            Self::EditProvider(_)
            | Self::RemoveProvider(_)
            | Self::SetText { .. }
            | Self::SetKind(_)
            | Self::ClearKey
            | Self::UseListedModels
            | Self::SetModel { .. }
            | Self::SetUnlimited(_)
            | Self::SaveLimits => "ai:edit-settings",
            Self::ToggleConnection => "ai:toggle-connection",
            Self::OpenAuditLog | Self::SetAuditFilter(_) | Self::SelectAuditEntry(_) => {
                "ai:open-audit-log"
            }
            Self::LoadMoreAudit => "ai:load-more-audit",
            Self::CopyAuditSql => "ai:copy-audit-sql",
        };
        CommandId::new(id).expect("constant command ID is valid")
    }
}

/// Registers the AI commands.
///
/// # Errors
///
/// Returns [`CommandError::Duplicate`] if one of the IDs is already registered.
pub fn register_commands(registry: &mut CommandRegistry) -> Result<(), CommandError> {
    for (id, title) in COMMANDS {
        registry.register(Command::new(id, *title)?)?;
    }
    Ok(())
}

/// The action a command stands for, or `None` when it is not an AI command or does not apply
/// now (`ai:save-provider` with no editor open, `ai:add-provider` with no provider compiled in).
#[must_use]
pub fn action_for(id: &CommandId, ws: &Workspace) -> Option<AiAction> {
    let editor_open = ws.ai().draft().is_some();
    match id.as_str() {
        "ai:open-settings" | "ai:edit-settings" => Some(AiAction::OpenSettings),
        "ai:add-provider" => ws.ai().kinds().first().copied().map(AiAction::AddProvider),
        "ai:test-provider" => Some(AiAction::TestProvider),
        "ai:save-provider" => editor_open.then_some(AiAction::SaveProvider),
        "ai:cancel-provider" => editor_open.then_some(AiAction::CancelProvider),
        "ai:toggle-connection" => Some(AiAction::ToggleConnection),
        "ai:open-audit-log" => Some(AiAction::OpenAuditLog),
        "ai:load-more-audit" => ws.audit().has_more().then_some(AiAction::LoadMoreAudit),
        "ai:copy-audit-sql" => ws
            .audit()
            .selected_sql()
            .is_some()
            .then_some(AiAction::CopyAuditSql),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiState;
    use crate::sidebar::SidebarModel;
    use sqler_config::AiConfig;

    fn pid(s: &str) -> ProviderId {
        ProviderId::new(s).unwrap()
    }

    fn all() -> Vec<AiAction> {
        vec![
            AiAction::OpenSettings,
            AiAction::AddProvider(ProviderKind::Ollama),
            AiAction::EditProvider(pid("a")),
            AiAction::RemoveProvider(pid("a")),
            AiAction::SetText {
                field: AiField::Endpoint,
                text: "x".into(),
            },
            AiAction::SetKind(ProviderKind::OpenAi),
            AiAction::ClearKey,
            AiAction::SaveProvider,
            AiAction::CancelProvider,
            AiAction::TestProvider,
            AiAction::UseListedModels,
            AiAction::SetModel {
                use_case: None,
                choice: None,
            },
            AiAction::SetUnlimited(true),
            AiAction::SaveLimits,
            AiAction::ToggleConnection,
            AiAction::OpenAuditLog,
            AiAction::LoadMoreAudit,
            AiAction::CopyAuditSql,
            AiAction::SetAuditFilter(AuditFilterChange::Tool(None)),
            AiAction::SelectAuditEntry(0),
        ]
    }

    #[test]
    fn ids_are_valid() {
        for (id, title) in COMMANDS {
            CommandId::new(id).unwrap();
            assert!(id.starts_with("ai:"));
            assert!(title.starts_with("AI: "));
        }
    }

    #[test]
    fn every_action_command_is_registered() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), COMMANDS.len());
        for action in all() {
            assert!(r.get(&action.command_id()).is_some(), "{action:?}");
        }
    }

    #[test]
    fn every_command_has_an_action() {
        let used: std::collections::HashSet<String> =
            all().iter().map(|a| a.command_id().to_string()).collect();
        for (id, _) in COMMANDS {
            assert!(used.contains(*id), "{id} has no action");
        }
    }

    #[test]
    fn registering_twice_is_a_duplicate_error() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert!(matches!(
            register_commands(&mut r),
            Err(CommandError::Duplicate(_))
        ));
    }

    fn act(id: &str, w: &Workspace) -> Option<AiAction> {
        action_for(&CommandId::new(id).unwrap(), w)
    }

    #[test]
    fn availability() {
        let mut w = Workspace::new(SidebarModel::default()).with_ai(AiState::new(
            AiConfig::default(),
            vec![ProviderKind::Ollama],
            None,
        ));
        assert_eq!(act("ai:open-settings", &w), Some(AiAction::OpenSettings));
        assert_eq!(act("ai:edit-settings", &w), Some(AiAction::OpenSettings));
        assert_eq!(
            act("ai:add-provider", &w),
            Some(AiAction::AddProvider(ProviderKind::Ollama))
        );
        assert_eq!(act("ai:test-provider", &w), Some(AiAction::TestProvider));
        assert_eq!(
            act("ai:toggle-connection", &w),
            Some(AiAction::ToggleConnection)
        );
        assert_eq!(act("ai:save-provider", &w), None);
        assert_eq!(act("ai:cancel-provider", &w), None);
        w.dispatch_ai(AiAction::AddProvider(ProviderKind::Ollama));
        assert_eq!(act("ai:save-provider", &w), Some(AiAction::SaveProvider));
        assert_eq!(
            act("ai:cancel-provider", &w),
            Some(AiAction::CancelProvider)
        );
    }

    #[test]
    fn audit_commands_apply_when_they_can_run() {
        let mut w = Workspace::new(SidebarModel::default());
        assert_eq!(act("ai:open-audit-log", &w), Some(AiAction::OpenAuditLog));
        assert_eq!(act("ai:load-more-audit", &w), None);
        assert_eq!(act("ai:copy-audit-sql", &w), None);
        assert_eq!(
            AiAction::SelectAuditEntry(3).command_id().as_str(),
            "ai:open-audit-log"
        );
        w.audit_fixture(crate::ai::audit::fixture::entries(1_800_000_000_000));
        assert_eq!(act("ai:copy-audit-sql", &w), None);
        w.dispatch_ai(AiAction::SelectAuditEntry(0));
        assert_eq!(act("ai:copy-audit-sql", &w), Some(AiAction::CopyAuditSql));
    }

    #[test]
    fn add_provider_needs_a_compiled_in_kind() {
        let w = Workspace::new(SidebarModel::default());
        assert_eq!(act("ai:add-provider", &w), None);
        let w = w.with_ai(AiState::for_config(AiConfig::default()));
        assert_eq!(
            act("ai:add-provider", &w),
            Some(AiAction::AddProvider(ProviderKind::Ollama))
        );
    }

    #[test]
    fn other_commands_are_not_ours() {
        let w = Workspace::new(SidebarModel::default());
        for id in ["tab:close", "ai:nope", "connection:new"] {
            assert_eq!(act(id, &w), None, "{id}");
        }
    }

    #[test]
    fn a_typed_key_is_not_in_debug_output() {
        let a = AiAction::SetText {
            field: AiField::Key,
            text: "sk-test-123".into(),
        };
        assert!(!format!("{a:?}").contains("sk-test-123"));
        let b = AiAction::SetText {
            field: AiField::Models,
            text: "m1".into(),
        };
        assert!(format!("{b:?}").contains("m1"));
    }
}
