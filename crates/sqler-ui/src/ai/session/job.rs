// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The jobs of an AI session tab (ADR-0053): building the session and running one turn. The view
//! runs both on tokio through `SessionBridge::spawn`; the session itself is lent to a turn and
//! comes back in `HostMsg::TurnDone`, like a query session comes back from a query job.

use std::sync::Arc;
use std::time::Duration;

use sqler_ai_api::AiProvider;
use sqler_ai_tools::session::{
    ExplorationSession, FocusTable, PromptKind, SchemaContextOptions, SessionConfig, SessionError,
    TurnOutcome,
};
use sqler_ai_tools::{Assist, AssistSearch, ConnectionInfo, Limits, Surface, ToolSet};
use sqler_config::{AiConfig, ProviderConfig};
use tokio::sync::mpsc::UnboundedSender;

use super::NO_TOOLS;
use super::host::{ChannelEvents, ChannelUiHost, HostMsg};
use super::source::TabSource;
use crate::ai::approval::ChannelApprover;
use crate::ai::{AiAudit, AiProviderFactory};
use crate::connections::{ConnectRequest, Connector};
use crate::tabs::TabId;

/// How strict the session is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionMode {
    /// An AI session tab: approval-and-run with the user's approval (ADR-0053).
    #[default]
    Tab,
    /// The chat panel (ADR-0076): read-only, always capped, no approver, no results tab.
    ChatPanel,
}

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
    /// Tab or chat panel.
    pub mode: SessionMode,
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
        mode,
        ..
    } = start;
    let built = tokio::task::spawn_blocking(move || factory.build(&provider, None))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())?;
    let parts = Parts {
        built,
        info,
        request,
        config,
        audit,
        connector,
        model,
    };
    let (source, session) = match mode {
        SessionMode::Tab => tab_session(parts, &tx),
        SessionMode::ChatPanel => chat_session(parts),
    }
    .map_err(|err| match err {
        SessionError::ToolsUnsupported => NO_TOOLS.to_owned(),
        other => other.to_string(),
    })?;
    Ok(SessionReady {
        session: session.with_events(Arc::new(ChannelEvents(tx))),
        source,
    })
}

/// What both session modes are built from.
struct Parts {
    built: Arc<dyn AiProvider>,
    info: ConnectionInfo,
    request: ConnectRequest,
    config: AiConfig,
    audit: AiAudit,
    connector: Arc<dyn Connector>,
    model: String,
}

type Built = Result<(Arc<TabSource>, ExplorationSession), SessionError>;

fn tab_session(p: Parts, tx: &UnboundedSender<HostMsg>) -> Built {
    let source = Arc::new(TabSource::new(p.info.clone(), p.request, p.connector));
    let tools = ToolSet::configured(source.clone(), &p.config)
        .with_ui_host(Arc::new(ChannelUiHost(tx.clone())));
    let tools = p.audit.attach(tools, Surface::Session);
    let approver = Arc::new(ChannelApprover::new(tx.clone(), HostMsg::Ask));
    let session = ExplorationSession::new(
        p.built,
        tools,
        p.info,
        Some(approver),
        SessionConfig::new(p.model),
    )?;
    Ok((source, session))
}

/// The chat panel (ADR-0076): the connection is read-only for the guard, so a statement that is
/// not a read is refused before any approval; there is no approver and no host for a results
/// tab; the caps are forced on whatever `unlimited` says.
fn chat_session(p: Parts) -> Built {
    let info = p.info.with_read_only(true);
    let source = Arc::new(TabSource::new(info.clone(), p.request, p.connector));
    let limits = &p.config.limits;
    let capped = Limits::new(
        limits.max_rows,
        Duration::from_secs(u64::from(limits.timeout_secs)),
        false,
    );
    let base = ToolSet::new(source.clone(), capped).with_redaction(p.config.redaction.clone());
    let assist = Assist::new(
        p.audit.attach(base.clone(), Surface::Chat),
        p.built.clone(),
        p.model.clone(),
    );
    let tools = p
        .audit
        .attach(base, Surface::Chat)
        .with_data_search(Arc::new(AssistSearch::new(assist)));
    let prompt = PromptKind::Chat {
        max_rows: capped.max_rows,
        timeout_secs: capped.timeout.as_secs(),
    };
    // The chat sends a compact schema of the connection with every turn (F3).
    let config = SessionConfig::new(p.model)
        .with_prompt(prompt)
        .with_schema_context(SchemaContextOptions::default());
    let session = ExplorationSession::new(p.built, tools, info, None, config)?;
    Ok((source, session))
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
    /// The tables the user is looking at; their schema is sent first. Empty for a tab.
    pub focus: Vec<FocusTable>,
}

// Never the message or the focus.
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
            focus,
            ..
        } = self;
        session.set_focus(focus);
        let outcome = session.send(text).await;
        // A closed channel means the tab is gone; the session is dropped with the message.
        let _ = tx.send(HostMsg::TurnDone(Box::new(TurnDone {
            turn,
            session,
            outcome,
        })));
    }
}
