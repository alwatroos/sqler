// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `AiAction`: every action of the AI settings and the AI view, each mapped to a command ID.
//! Later AI features add their rows here (ADR-0050).

use sqler_ai_tools::session::TranscriptEntry;
use sqler_config::{ModelChoice, ProviderId, ProviderKind, UseCase};

use super::audit::AuditFilterChange;
use super::settings::AiField;
use crate::command::{Command, CommandError, CommandId, CommandRegistry};
use crate::tabs::TabId;
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
    /// Open the prompt bar of the active SQL editor (or keep it open).
    GenerateSql,
    /// Store the question typed into the prompt bar.
    SetPrompt(String),
    /// Send the question of the prompt bar.
    SubmitPrompt,
    /// Close the prompt bar; a request that runs is cancelled.
    ClosePrompt,
    /// Explain the selection, else the statement at the cursor.
    ExplainQuery,
    /// Fix the first failed statement of the tab.
    FixQuery,
    /// Fix this statement of the last run (its position in the results).
    FixStatement(usize),
    /// Cancel the request that runs in the tab.
    CancelAssist,
    /// Replace the failed statement in the editor with the proposed fix.
    AcceptFix,
    /// Dismiss the proposed fix.
    RejectFix,
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
    /// Open an AI session tab on the connection the user works on.
    OpenSession,
    /// Set the text in the composer of an AI session tab.
    SetSessionDraft(TabId, String),
    /// Send the composer text of an AI session tab to the model.
    SendMessage(TabId),
    /// Cancel the running turn of an AI session tab.
    CancelSession(TabId),
    /// Approve the statement with this ask id that waits in an AI session tab. It answers only
    /// that ask, and only while it is the one shown.
    ApproveStatement(TabId, u64),
    /// Deny the statement with this ask id that waits in an AI session tab.
    DenyStatement(TabId, u64),
    /// Run a statement of the transcript again (stable entry number) in a new editor tab.
    RerunSql(TabId, usize),
    /// Open a statement of the transcript (stable entry number) in a new editor tab.
    OpenSqlInEditor(TabId, usize),
}

impl AiAction {
    /// Whether the AI session tab handles this action.
    #[must_use]
    pub fn is_session(&self) -> bool {
        matches!(
            self,
            Self::OpenSession
                | Self::SetSessionDraft(..)
                | Self::SendMessage(_)
                | Self::CancelSession(_)
                | Self::ApproveStatement(..)
                | Self::DenyStatement(..)
                | Self::RerunSql(..)
                | Self::OpenSqlInEditor(..)
        )
    }
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
            Self::GenerateSql => f.write_str("GenerateSql"),
            // The question is the user's text: its length only.
            Self::SetPrompt(text) => f
                .debug_struct("SetPrompt")
                .field("len", &text.len())
                .finish(),
            Self::SubmitPrompt => f.write_str("SubmitPrompt"),
            Self::ClosePrompt => f.write_str("ClosePrompt"),
            Self::ExplainQuery => f.write_str("ExplainQuery"),
            Self::FixQuery => f.write_str("FixQuery"),
            Self::FixStatement(i) => f.debug_tuple("FixStatement").field(i).finish(),
            Self::CancelAssist => f.write_str("CancelAssist"),
            Self::AcceptFix => f.write_str("AcceptFix"),
            Self::RejectFix => f.write_str("RejectFix"),
            Self::OpenAuditLog => f.write_str("OpenAuditLog"),
            Self::LoadMoreAudit => f.write_str("LoadMoreAudit"),
            Self::CopyAuditSql => f.write_str("CopyAuditSql"),
            Self::SetAuditFilter(change) => f.debug_tuple("SetAuditFilter").field(change).finish(),
            Self::SelectAuditEntry(ix) => f.debug_tuple("SelectAuditEntry").field(ix).finish(),
            Self::OpenSession => f.write_str("OpenSession"),
            // The length only: the draft is the user's message.
            Self::SetSessionDraft(tab, text) => f
                .debug_struct("SetSessionDraft")
                .field("tab", tab)
                .field("len", &text.len())
                .finish(),
            Self::SendMessage(tab) => f.debug_tuple("SendMessage").field(tab).finish(),
            Self::CancelSession(tab) => f.debug_tuple("CancelSession").field(tab).finish(),
            Self::ApproveStatement(tab, id) => f
                .debug_tuple("ApproveStatement")
                .field(tab)
                .field(id)
                .finish(),
            Self::DenyStatement(tab, id) => {
                f.debug_tuple("DenyStatement").field(tab).field(id).finish()
            }
            Self::RerunSql(tab, ix) => f.debug_tuple("RerunSql").field(tab).field(ix).finish(),
            Self::OpenSqlInEditor(tab, ix) => f
                .debug_tuple("OpenSqlInEditor")
                .field(tab)
                .field(ix)
                .finish(),
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
    ("ai:generate-sql", "AI: Generate SQL"),
    ("ai:explain-query", "AI: Explain Query"),
    ("ai:fix-query", "AI: Fix Failed Query"),
    ("ai:cancel-assist", "AI: Cancel Request"),
    ("ai:accept-fix", "AI: Accept Fix"),
    ("ai:reject-fix", "AI: Reject Fix"),
    ("ai:open-audit-log", "AI: Open Audit Log"),
    ("ai:load-more-audit", "AI: Load More Audit Entries"),
    ("ai:copy-audit-sql", "AI: Copy SQL of Audit Entry"),
    ("ai:open-session", "AI: Open Session"),
    ("ai:send-message", "AI: Send Message"),
    ("ai:cancel-session", "AI: Cancel Session Turn"),
    ("ai:approve-statement", "AI: Approve Statement"),
    ("ai:deny-statement", "AI: Deny Statement"),
    ("ai:rerun-sql", "AI: Re-run SQL"),
    ("ai:open-sql-in-editor", "AI: Open SQL in Editor"),
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
            Self::GenerateSql | Self::SetPrompt(_) | Self::SubmitPrompt | Self::ClosePrompt => {
                "ai:generate-sql"
            }
            Self::ExplainQuery => "ai:explain-query",
            Self::FixQuery | Self::FixStatement(_) => "ai:fix-query",
            Self::CancelAssist => "ai:cancel-assist",
            Self::AcceptFix => "ai:accept-fix",
            Self::RejectFix => "ai:reject-fix",
            Self::OpenAuditLog | Self::SetAuditFilter(_) | Self::SelectAuditEntry(_) => {
                "ai:open-audit-log"
            }
            Self::LoadMoreAudit => "ai:load-more-audit",
            Self::CopyAuditSql => "ai:copy-audit-sql",
            Self::OpenSession => "ai:open-session",
            Self::SetSessionDraft(..) | Self::SendMessage(_) => "ai:send-message",
            Self::CancelSession(_) => "ai:cancel-session",
            Self::ApproveStatement(..) => "ai:approve-statement",
            Self::DenyStatement(..) => "ai:deny-statement",
            Self::RerunSql(..) => "ai:rerun-sql",
            Self::OpenSqlInEditor(..) => "ai:open-sql-in-editor",
        };
        CommandId::new(id).expect("constant command ID is valid")
    }

    /// Whether the editor assistant handles the action (`Workspace::dispatch_assist`).
    #[must_use]
    pub fn is_assist(&self) -> bool {
        matches!(
            self,
            Self::GenerateSql
                | Self::SetPrompt(_)
                | Self::SubmitPrompt
                | Self::ClosePrompt
                | Self::ExplainQuery
                | Self::FixQuery
                | Self::FixStatement(_)
                | Self::CancelAssist
                | Self::AcceptFix
                | Self::RejectFix
        )
    }

    /// Whether the action needs the editor text (the view passes the buffer of the active tab).
    #[must_use]
    pub fn needs_buffer(&self) -> bool {
        matches!(self, Self::ExplainQuery | Self::AcceptFix)
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
        "ai:generate-sql" => ws.assist_tab().map(|_| AiAction::GenerateSql),
        "ai:explain-query" => ws.assist_tab().map(|_| AiAction::ExplainQuery),
        "ai:fix-query" => ws.assist_tab().map(|_| AiAction::FixQuery),
        "ai:cancel-assist" => ws
            .assist_tab()
            .filter(|tab| ws.assist_running(*tab))
            .map(|_| AiAction::CancelAssist),
        "ai:accept-fix" => ws
            .assist_tab()
            .filter(|tab| ws.assist_has_fix(*tab))
            .map(|_| AiAction::AcceptFix),
        "ai:reject-fix" => ws
            .assist_tab()
            .filter(|tab| ws.assist_has_fix(*tab))
            .map(|_| AiAction::RejectFix),
        "ai:open-audit-log" => Some(AiAction::OpenAuditLog),
        "ai:load-more-audit" => ws.audit().has_more().then_some(AiAction::LoadMoreAudit),
        "ai:copy-audit-sql" => ws
            .audit()
            .selected_sql()
            .is_some()
            .then_some(AiAction::CopyAuditSql),
        "ai:open-session" => Some(AiAction::OpenSession),
        "ai:send-message"
        | "ai:cancel-session"
        | "ai:approve-statement"
        | "ai:deny-statement"
        | "ai:rerun-sql"
        | "ai:open-sql-in-editor" => session_action(id.as_str(), ws),
        _ => None,
    }
}

/// The session tab commands: they apply only while the active tab is an AI session.
fn session_action(id: &str, ws: &Workspace) -> Option<AiAction> {
    let (tab, session) = ws.active_ai_session()?;
    let last_sql = || {
        session
            .entries()
            .iter()
            .rposition(|e| matches!(e, TranscriptEntry::Sql(_)))
            .map(|ix| session.dropped() + ix)
    };
    let shown = || {
        session
            .approvals()
            .shown_current(sqler_ai_tools::ApprovalStage::Confirm)
    };
    match id {
        "ai:send-message" => Some(AiAction::SendMessage(tab)),
        "ai:cancel-session" => session.is_running().then_some(AiAction::CancelSession(tab)),
        // Only the ask whose card the user has seen with its exact SQL (ADR-0053).
        "ai:approve-statement" => shown().map(|id| AiAction::ApproveStatement(tab, id)),
        "ai:deny-statement" => shown().map(|id| AiAction::DenyStatement(tab, id)),
        "ai:rerun-sql" => last_sql().map(|ix| AiAction::RerunSql(tab, ix)),
        "ai:open-sql-in-editor" => last_sql().map(|ix| AiAction::OpenSqlInEditor(tab, ix)),
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

    fn tab() -> TabId {
        let conn = crate::tabs::TabConnection {
            name: "db".into(),
            driver: sqler_driver_api::sqler_core::DriverId::new("sqlite").unwrap(),
            color: None,
            environment: None,
        };
        crate::tabs::TabStrip::default().open(conn, crate::tabs::TabKind::AiSession)
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
            AiAction::GenerateSql,
            AiAction::SetPrompt("q".into()),
            AiAction::SubmitPrompt,
            AiAction::ClosePrompt,
            AiAction::ExplainQuery,
            AiAction::FixQuery,
            AiAction::FixStatement(0),
            AiAction::CancelAssist,
            AiAction::AcceptFix,
            AiAction::RejectFix,
            AiAction::OpenAuditLog,
            AiAction::LoadMoreAudit,
            AiAction::CopyAuditSql,
            AiAction::SetAuditFilter(AuditFilterChange::Tool(None)),
            AiAction::SelectAuditEntry(0),
            AiAction::OpenSession,
            AiAction::SetSessionDraft(tab(), String::new()),
            AiAction::SendMessage(tab()),
            AiAction::CancelSession(tab()),
            AiAction::ApproveStatement(tab(), 0),
            AiAction::DenyStatement(tab(), 0),
            AiAction::RerunSql(tab(), 0),
            AiAction::OpenSqlInEditor(tab(), 0),
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
    fn assist_commands_need_a_query_tab() {
        let w = Workspace::new(SidebarModel::default());
        for id in [
            "ai:generate-sql",
            "ai:explain-query",
            "ai:fix-query",
            "ai:cancel-assist",
            "ai:accept-fix",
            "ai:reject-fix",
        ] {
            assert_eq!(act(id, &w), None, "{id}");
        }
    }

    #[test]
    fn assist_actions_are_marked() {
        for a in all() {
            let assist = a.command_id().as_str().starts_with("ai:")
                && !a.is_session()
                && !matches!(
                    a.command_id().as_str(),
                    "ai:open-settings"
                        | "ai:add-provider"
                        | "ai:test-provider"
                        | "ai:save-provider"
                        | "ai:cancel-provider"
                        | "ai:edit-settings"
                        | "ai:toggle-connection"
                        | "ai:open-audit-log"
                        | "ai:load-more-audit"
                        | "ai:copy-audit-sql"
                );
            assert_eq!(a.is_assist(), assist, "{a:?}");
        }
        assert!(AiAction::ExplainQuery.needs_buffer());
        assert!(AiAction::AcceptFix.needs_buffer());
        assert!(!AiAction::GenerateSql.needs_buffer());
        assert!(!AiAction::FixQuery.needs_buffer());
    }

    #[test]
    fn a_typed_question_is_not_in_debug_output() {
        let text = format!("{:?}", AiAction::SetPrompt("CANARY-QUESTION".into()));
        assert!(!text.contains("CANARY"), "{text}");
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

    #[test]
    fn session_actions_are_ours_and_hide_the_message() {
        let secret = "canary message";
        let draft = AiAction::SetSessionDraft(tab(), secret.into());
        assert!(draft.is_session());
        let text = format!("{draft:?}");
        assert!(!text.contains(secret));
        assert!(text.contains("len"));
        assert!(AiAction::OpenSession.is_session());
        assert!(!AiAction::ToggleConnection.is_session());
        assert_eq!(
            AiAction::SetSessionDraft(tab(), String::new())
                .command_id()
                .as_str(),
            "ai:send-message"
        );
    }

    #[test]
    fn session_commands_need_a_session_tab() {
        let w = Workspace::new(SidebarModel::default());
        assert_eq!(act("ai:open-session", &w), Some(AiAction::OpenSession));
        for id in [
            "ai:send-message",
            "ai:cancel-session",
            "ai:approve-statement",
            "ai:deny-statement",
            "ai:rerun-sql",
            "ai:open-sql-in-editor",
        ] {
            assert_eq!(act(id, &w), None, "{id}");
        }
    }
}
