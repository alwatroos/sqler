// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The jobs of an AI session tab (ADR-0053): building the session and running one turn. The view
//! runs both on tokio through `SessionBridge::spawn`; the session itself is lent to a turn and
//! comes back in `HostMsg::TurnDone`, like a query session comes back from a query job.

use std::sync::Arc;

use sqler_ai_tools::session::{ExplorationSession, SessionConfig, SessionError, TurnOutcome};
use sqler_ai_tools::{ConnectionInfo, Surface, ToolSet};
use sqler_config::{AiConfig, ProviderConfig};
use tokio::sync::mpsc::UnboundedSender;

use super::NO_TOOLS;
use super::host::{ChannelEvents, ChannelUiHost, HostMsg};
use super::source::TabSource;
use crate::ai::approval::ChannelApprover;
use crate::ai::{AiAudit, AiProviderFactory};
use crate::connections::{ConnectRequest, Connector};
use crate::tabs::TabId;

/// Everything needed to build the session of a new tab.
pub struct AiSessionStart {
    /// The tab.
    pub tab: TabId,
    /// The provider to talk to.
    pub provider: ProviderConfig,
    /// The model.
    pub model: String,
    /// The connection the session explores.
    pub info: ConnectionInfo,
    /// How to open it.
    pub request: ConnectRequest,
    /// Limits and the redaction list.
    pub config: AiConfig,
    /// Where the tool calls are audited (required).
    pub audit: AiAudit,
    /// The host channel of the tab.
    pub tx: UnboundedSender<HostMsg>,
}

// Ids only: never the endpoint, the model or the connection details.
impl std::fmt::Debug for AiSessionStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiSessionStart")
            .field("tab", &self.tab)
            .field("provider", &self.provider.id.as_str())
            .finish_non_exhaustive()
    }
}

/// A built session and the source behind its tools.
pub struct SessionReady {
    /// The idle session.
    pub session: ExplorationSession,
    /// The source the tools read through.
    pub source: Arc<TabSource>,
}

impl std::fmt::Debug for SessionReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionReady")
    }
}

/// Builds the provider (on the blocking pool, it may read the keychain) and the session.
///
/// # Errors
/// The reason as text: the provider could not be built, or it cannot call tools ([`NO_TOOLS`]).
pub async fn build_session(
    start: AiSessionStart,
    factory: Arc<dyn AiProviderFactory>,
    connector: Arc<dyn Connector>,
) -> Result<SessionReady, String> {
    let AiSessionStart {
        provider,
        model,
        info,
        request,
        config,
        audit,
        tx,
        ..
    } = start;
    let built = tokio::task::spawn_blocking(move || factory.build(&provider, None))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())?;
    let source = Arc::new(TabSource::new(info.clone(), request, connector));
    let tools = ToolSet::configured(source.clone(), &config)
        .with_ui_host(Arc::new(ChannelUiHost(tx.clone())));
    let tools = audit.attach(tools, Surface::Session);
    let approver = Arc::new(ChannelApprover::new(tx.clone(), HostMsg::Ask));
    let session = ExplorationSession::new(
        built,
        tools,
        info,
        Some(approver),
        SessionConfig::new(model),
    )
    .map_err(|err| match err {
        SessionError::ToolsUnsupported => NO_TOOLS.to_owned(),
        other => other.to_string(),
    })?
    .with_events(Arc::new(ChannelEvents(tx)));
    Ok(SessionReady { session, source })
}

/// One user turn. Lends the session; it comes back in [`TurnDone`].
pub struct AiTurnJob {
    /// The tab.
    pub tab: TabId,
    /// Tells this turn from an older one.
    pub turn: u64,
    /// The session, idle until now.
    pub session: ExplorationSession,
    /// What the user typed.
    pub text: String,
    /// The host channel of the tab.
    pub tx: UnboundedSender<HostMsg>,
}

// Never the message.
impl std::fmt::Debug for AiTurnJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiTurnJob")
            .field("tab", &self.tab)
            .field("turn", &self.turn)
            .finish_non_exhaustive()
    }
}

/// The end of a turn and the session it gives back.
pub struct TurnDone {
    /// The turn that ended.
    pub turn: u64,
    /// The session, idle again.
    pub session: ExplorationSession,
    /// How the turn ended.
    pub outcome: TurnOutcome,
}

impl std::fmt::Debug for TurnDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnDone")
            .field("turn", &self.turn)
            .finish_non_exhaustive()
    }
}

impl AiTurnJob {
    /// Runs the turn, then reports it on the host channel. Dropping the future cancels the turn.
    pub async fn run(self) {
        let Self {
            turn,
            mut session,
            text,
            tx,
            ..
        } = self;
        let outcome = session.send(text).await;
        // A closed channel means the tab is gone; the session is dropped with the message.
        let _ = tx.send(HostMsg::TurnDone(Box::new(TurnDone {
            turn,
            session,
            outcome,
        })));
    }
}
