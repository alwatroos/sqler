// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! AI in the workbench (ADR-0050): the settings state, the commands, the status indicator and
//! the AI side view model. Plain Rust, no `GPUI`; the views live behind the `gui` feature.
//!
//! Providers are reached only through `sqler-ai-api` and an [`AiProviderFactory`] that the binary
//! implements per cargo feature. Keys live in the OS keychain only.

mod action;
pub mod approval;
pub mod audit;
pub mod editor;
pub mod hub;
pub mod indicator;
mod models;
mod provider;
pub mod session;
pub mod settings;
pub(crate) mod state;

pub use action::{AiAction, COMMANDS, action_for, register_commands};
pub use approval::{
    ApprovalAsk, ApprovalQueue, ChannelApprover, PendingApproval, card_title, open_prod_gate,
};
pub use audit::{
    AUDIT_DECISIONS, AUDIT_PAGE, AUDIT_SURFACES, AUDIT_TOOLS, AuditBatch, AuditFilterChange,
    AuditFilters, AuditLoad, AuditPanel, AuditRead, AuditRow, AuditTone, audit_row, decision_label,
    entry_sql, surface_label, wire_label,
};
pub use editor::{
    AssistJob, AssistKind, AssistOutcome, AssistRequest, AssistState, EditorAssist, EditorSource,
    PromptBar, TabAssist, run_assist,
};
pub use hub::{HubConnection, HubModel, ProviderRow, SessionRow, hub_model};
pub use indicator::{AiStatus, ai_status, is_local, main_choice};
pub use models::{ModelRow, model_choices, model_rows, use_case_title};
pub use provider::{AiProviderFactory, AiStartup, NoProviders};
pub use session::{AiSessionTab, AiSessions, HostMsg, SessionPhase};
pub use settings::{
    AiField, AiTestStatus, DraftError, KeyPlan, LimitsDraft, ProviderDraft, needs_key,
};
pub use state::AiState;

use sqler_config::ProviderKind;

/// The name of a provider kind for people (`OpenAI`, not `openai`).
#[must_use]
pub fn kind_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Ollama => "Ollama",
        ProviderKind::Anthropic => "Anthropic",
        ProviderKind::OpenAi => "OpenAI",
        ProviderKind::Mistral => "Mistral",
        _ => "AI",
    }
}

/// Every provider kind, in the order the settings offer them.
pub const ALL_KINDS: [ProviderKind; 4] = [
    ProviderKind::Ollama,
    ProviderKind::Anthropic,
    ProviderKind::OpenAi,
    ProviderKind::Mistral,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_readable() {
        assert_eq!(kind_label(ProviderKind::OpenAi), "OpenAI");
        for kind in ALL_KINDS {
            assert_ne!(kind_label(kind), "");
            assert_eq!(ProviderKind::from_id(kind.as_str()), Some(kind));
        }
    }
}
