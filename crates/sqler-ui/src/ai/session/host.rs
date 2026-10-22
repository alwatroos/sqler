// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The channel between a running session and the workspace (ADR-0053).
//!
//! One ordered channel per AI session tab carries everything the session tells the UI: engine
//! events, approval asks, calls to the UI tools and the end of the turn. The view drains it into
//! [`Workspace::ai_session_message`](crate::workspace::Workspace::ai_session_message), so the
//! order is exact and every flow can be tested without `GPUI`.

use sqler_ai_tools::session::{SessionEvent, SessionEvents};
use sqler_ai_tools::{ToolError, UiHost};
use sqler_driver_api::BoxFuture;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use super::TAB_CLOSED;
use super::job::TurnDone;
use crate::ai::approval::ApprovalAsk;

/// What a running session tells the workspace.
pub enum HostMsg {
    /// An engine event (state, text, transcript entry, end of turn).
    Event(SessionEvent),
    /// A statement waits for the user.
    Ask(ApprovalAsk),
    /// The model called a tool that only the UI can run.
    Ui(UiCall),
    /// The turn ended and gives the session back.
    TurnDone(Box<TurnDone>),
}

// Never the text of an event, the SQL of an ask or a call.
impl std::fmt::Debug for HostMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Event(_) => f.write_str("HostMsg::Event(..)"),
            Self::Ask(ask) => f.debug_tuple("HostMsg::Ask").field(ask).finish(),
            Self::Ui(call) => f.debug_tuple("HostMsg::Ui").field(call).finish(),
            Self::TurnDone(done) => f.debug_tuple("HostMsg::TurnDone").field(done).finish(),
        }
    }
}

/// What the model asks the UI to do.
#[derive(Clone, PartialEq, Eq)]
pub enum UiRequest {
    /// Put SQL in a new editor tab.
    OpenInEditor {
        /// The connection name or id, if the model named one.
        connection: Option<String>,
        /// The SQL to seed the editor with.
        sql: String,
    },
    /// Run SQL and show the rows in a results tab.
    OpenResults {
        /// The connection name or id.
        connection: String,
        /// The (read-only) SQL.
        sql: String,
        /// The tab title, if the model gave one.
        title: Option<String>,
    },
}

// Never the SQL.
impl std::fmt::Debug for UiRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenInEditor { .. } => f.write_str("OpenInEditor"),
            Self::OpenResults { .. } => f.write_str("OpenResults"),
        }
    }
}

/// A call of a UI tool and the way to answer it.
pub struct UiCall {
    /// What to do.
    pub request: UiRequest,
    /// Where the result goes. Dropping it answers with an error (the tab is gone).
    pub reply: oneshot::Sender<Result<(), ToolError>>,
}

impl std::fmt::Debug for UiCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiCall")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

/// Sends the engine events of a session on the host channel.
pub struct ChannelEvents(pub UnboundedSender<HostMsg>);

impl SessionEvents for ChannelEvents {
    fn emit(&self, event: SessionEvent) {
        // A closed channel means the tab is gone; nobody needs the event.
        let _ = self.0.send(HostMsg::Event(event));
    }
}

impl std::fmt::Debug for ChannelEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelEvents")
    }
}

/// Forwards the UI tools of the model to the workspace on the host channel.
pub struct ChannelUiHost(pub UnboundedSender<HostMsg>);

impl std::fmt::Debug for ChannelUiHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelUiHost")
    }
}

impl ChannelUiHost {
    async fn call(&self, request: UiRequest) -> Result<(), ToolError> {
        let (reply, answer) = oneshot::channel();
        if self.0.send(HostMsg::Ui(UiCall { request, reply })).is_err() {
            return Err(ToolError::Host(TAB_CLOSED.to_owned()));
        }
        answer
            .await
            .unwrap_or_else(|_| Err(ToolError::Host(TAB_CLOSED.to_owned())))
    }
}

impl UiHost for ChannelUiHost {
    fn open_in_editor<'a>(
        &'a self,
        connection: Option<&'a str>,
        sql: &'a str,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        Box::pin(self.call(UiRequest::OpenInEditor {
            connection: connection.map(str::to_owned),
            sql: sql.to_owned(),
        }))
    }

    fn open_results_tab<'a>(
        &'a self,
        connection: &'a str,
        sql: &'a str,
        title: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ToolError>> {
        Box::pin(self.call(UiRequest::OpenResults {
            connection: connection.to_owned(),
            sql: sql.to_owned(),
            title: title.map(str::to_owned),
        }))
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    #[tokio::test]
    async fn a_ui_call_waits_for_the_reply() {
        let (tx, mut rx) = unbounded_channel();
        let host = ChannelUiHost(tx);
        let call = tokio::spawn(async move { host.open_in_editor(Some("db"), "select 1").await });
        let Some(HostMsg::Ui(ui)) = rx.recv().await else {
            panic!("no ui call");
        };
        assert_eq!(
            ui.request,
            UiRequest::OpenInEditor {
                connection: Some("db".into()),
                sql: "select 1".into()
            }
        );
        ui.reply.send(Ok(())).unwrap();
        assert!(call.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn a_dropped_reply_or_receiver_is_tab_closed() {
        let (tx, mut rx) = unbounded_channel();
        let host = ChannelUiHost(tx);
        let call = tokio::spawn(async move { host.open_results_tab("db", "select 1", None).await });
        let Some(HostMsg::Ui(ui)) = rx.recv().await else {
            panic!("no ui call");
        };
        drop(ui);
        let err = call.await.unwrap().unwrap_err();
        assert!(matches!(err, ToolError::Host(m) if m == TAB_CLOSED));

        let (tx, rx) = unbounded_channel();
        drop(rx);
        let err = ChannelUiHost(tx)
            .open_in_editor(None, "x")
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Host(m) if m == TAB_CLOSED));
    }

    #[test]
    fn debug_hides_the_sql() {
        let (reply, _rx) = oneshot::channel();
        let call = UiCall {
            request: UiRequest::OpenResults {
                connection: "db".into(),
                sql: "select canary".into(),
                title: Some("canary-title".into()),
            },
            reply,
        };
        let text = format!("{:?}", HostMsg::Ui(call));
        assert!(!text.contains("canary"));
        let event = HostMsg::Event(SessionEvent::TextDelta("canary text".into()));
        assert!(!format!("{event:?}").contains("canary"));
    }
}
