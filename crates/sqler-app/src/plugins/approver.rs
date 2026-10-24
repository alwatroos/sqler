// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plugin host's [`QueryApprover`] (AC6): every ask becomes a message to the window, which shows the exact
//! statement and waits for the user. The host asks `Confirm` and, on a production connection, `ProdSecond` as well;
//! each stage is one ask here, and the window draws the second one on the production gate of the query editor
//! (ADR-0024). A closed channel, a closed window and a dropped reply all mean "not approved".

use sqler_driver_api::BoxFuture;
use sqler_driver_api::sqler_core::Environment;
use sqler_plugin_host::{ApprovalStage, Decision, QueryApprover, WriteRequest};
use sqler_ui::plugins::{PluginWriteAsk, PluginWriteRequest, WriteSender, WriteStage};
use tokio::sync::oneshot;

/// Sends the asks to the window.
pub(super) struct UiApprover {
    tx: WriteSender,
}

impl UiApprover {
    pub(super) fn new(tx: WriteSender) -> Self {
        Self { tx }
    }
}

/// What the window shows for a host request.
fn ui_request(request: &WriteRequest) -> PluginWriteRequest {
    let stage = match request.stage {
        ApprovalStage::ProdSecond => WriteStage::ProdSecond,
        _ => WriteStage::First,
    };
    PluginWriteRequest::new(
        request.plugin.clone(),
        request.plugin_name.clone(),
        request.connection.name.clone(),
        request.connection.driver.clone(),
        request.connection.environment == Some(Environment::Prod),
        request.sql.clone(),
        stage,
    )
    .with_dialect(request.connection.dialect)
    .with_param_count(request.params.len())
}

impl QueryApprover for UiApprover {
    fn approve<'a>(&'a self, request: &'a WriteRequest) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            let ask = PluginWriteAsk {
                request: ui_request(request),
                reply,
            };
            if self.tx.send(ask).is_err() {
                return Decision::Deny;
            }
            match answer.await {
                Ok(true) => Decision::Approve,
                _ => Decision::Deny,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqler_driver_api::sqler_core::DriverId;
    use sqler_plugin_host::{PluginConnection, WriteReason};
    use sqler_sql::StatementClass;
    use sqler_ui::plugins::write_channel;

    use super::*;

    fn request(stage: ApprovalStage, env: Option<Environment>) -> WriteRequest {
        let conn = PluginConnection::new("c1", "Orders", DriverId::new("sqlite").unwrap(), env);
        WriteRequest::new(
            "acme-sync".parse().unwrap(),
            "Acme Sync",
            conn,
            "update t set a = 1",
            Vec::new(),
            StatementClass::Write,
            WriteReason::Write,
            stage,
        )
    }

    async fn decide(answer: impl FnOnce(PluginWriteAsk) + Send + 'static) -> Decision {
        let (tx, inbox) = write_channel();
        let mut rx = inbox.take().unwrap();
        let user = tokio::spawn(async move {
            if let Some(ask) = rx.recv().await {
                answer(ask);
            }
        });
        let approver = UiApprover::new(tx);
        let decision = tokio::time::timeout(
            Duration::from_secs(10),
            approver.approve(&request(ApprovalStage::Confirm, None)),
        )
        .await
        .unwrap();
        user.await.unwrap();
        decision
    }

    #[tokio::test]
    async fn approve_and_deny_follow_the_answer() {
        assert_eq!(
            decide(|ask| {
                assert_eq!(ask.request.stage, WriteStage::First);
                assert_eq!(ask.request.sql, "update t set a = 1");
                assert!(!ask.request.prod);
                ask.reply.send(true).unwrap();
            })
            .await,
            Decision::Approve
        );
        assert_eq!(
            decide(|ask| {
                ask.reply.send(false).unwrap();
            })
            .await,
            Decision::Deny
        );
    }

    #[tokio::test]
    async fn a_dropped_reply_denies() {
        assert_eq!(decide(drop).await, Decision::Deny);
    }

    #[tokio::test]
    async fn a_closed_channel_denies() {
        let (tx, inbox) = write_channel();
        drop(inbox.take());
        let approver = UiApprover::new(tx);
        let decision = approver
            .approve(&request(ApprovalStage::Confirm, None))
            .await;
        assert_eq!(decision, Decision::Deny);
    }

    #[tokio::test]
    async fn a_production_write_is_asked_twice_with_the_exact_sql() {
        let (tx, inbox) = write_channel();
        let mut rx = inbox.take().unwrap();
        let user = tokio::spawn(async move {
            let mut stages = Vec::new();
            while let Some(ask) = rx.recv().await {
                assert!(ask.request.prod);
                assert_eq!(ask.request.sql, "update t set a = 1");
                stages.push(ask.request.stage);
                ask.reply.send(true).unwrap();
            }
            stages
        });
        let approver = UiApprover::new(tx);
        for stage in [ApprovalStage::Confirm, ApprovalStage::ProdSecond] {
            let request = request(stage, Some(Environment::Prod));
            assert_eq!(approver.approve(&request).await, Decision::Approve);
        }
        drop(approver);
        let stages = user.await.unwrap();
        assert_eq!(stages, [WriteStage::First, WriteStage::ProdSecond]);
    }
}
