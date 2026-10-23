// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Approvals of plugin writes (ADR-0058, ADR-0060), after the AI statement approvals (ADR-0053).
//!
//! The app's approver turns each question of the plugin host into a [`PluginWriteAsk`] on a channel the workspace
//! drains; the [`WriteQueue`] keeps the asks in order until the user answers. Plain Rust: no `GPUI`. A dropped reply,
//! a closed channel and a window that closes always mean "deny".
//!
//! The host asks twice on a production connection (ADR-0024): [`WriteStage::First`], then
//! [`WriteStage::ProdSecond`]. The second one goes through the run gate of the query editor: the queue owns a
//! [`QueryTab`] it opens with [`open_prod_gate`] on the exact SQL, and the view draws the gate's confirmation.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sqler_driver_api::sqler_core::DriverId;
use sqler_plugin_api::PluginId;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use crate::ai::open_prod_gate;
use crate::query::{BoundStatement, Phase, QueryTab};

#[cfg(feature = "gui")]
pub(crate) mod view;

/// Which confirmation is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteStage {
    /// The first confirmation: the exact statement and the plugin that wants it.
    First,
    /// The second confirmation every statement on a production connection needs (the run gate).
    ProdSecond,
}

/// A statement a plugin wants to run, as the user sees it.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PluginWriteRequest {
    /// The plugin that asks.
    pub plugin: PluginId,
    /// Its display name.
    pub plugin_name: String,
    /// The display name of the connection.
    pub connection: String,
    /// The driver of the connection (picks the SQL dialect of the gate).
    pub driver: DriverId,
    /// Whether the connection is labelled production.
    pub prod: bool,
    /// The exact text that will be sent to the driver.
    pub sql: String,
    /// How many parameters will be bound with it.
    pub param_count: usize,
    /// Which confirmation this is.
    pub stage: WriteStage,
}

impl PluginWriteRequest {
    /// A request without parameters.
    #[must_use]
    pub fn new(
        plugin: PluginId,
        plugin_name: impl Into<String>,
        connection: impl Into<String>,
        driver: DriverId,
        prod: bool,
        sql: impl Into<String>,
        stage: WriteStage,
    ) -> Self {
        Self {
            plugin,
            plugin_name: plugin_name.into(),
            connection: connection.into(),
            driver,
            prod,
            sql: sql.into(),
            param_count: 0,
            stage,
        }
    }

    /// Sets how many parameters are bound with the statement.
    #[must_use]
    pub fn with_param_count(mut self, count: usize) -> Self {
        self.param_count = count;
        self
    }
}

// Never the SQL, the connection or the plugin's text.
impl std::fmt::Debug for PluginWriteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginWriteRequest")
            .field("plugin", &self.plugin)
            .field("stage", &self.stage)
            .field("prod", &self.prod)
            .finish_non_exhaustive()
    }
}

/// One question to the user: the request and the way to answer it (`true` = approve; dropping it denies).
pub struct PluginWriteAsk {
    /// What the plugin wants to run.
    pub request: PluginWriteRequest,
    /// Where the answer goes.
    pub reply: oneshot::Sender<bool>,
}

impl std::fmt::Debug for PluginWriteAsk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginWriteAsk")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

/// The sending side of the channel; the app's approver holds it.
pub type WriteSender = UnboundedSender<PluginWriteAsk>;

/// The receiving side, handed to the workspace through `PluginsStartup`. The workspace takes it once.
#[derive(Clone, Default)]
pub struct WriteInbox(Arc<Mutex<Option<UnboundedReceiver<PluginWriteAsk>>>>);

impl WriteInbox {
    /// Takes the receiver. `None` when it was taken already or there never was one.
    #[must_use]
    pub fn take(&self) -> Option<UnboundedReceiver<PluginWriteAsk>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    /// Whether a receiver waits to be taken.
    #[must_use]
    pub fn is_set(&self) -> bool {
        let guard: MutexGuard<'_, _> = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        guard.is_some()
    }
}

impl std::fmt::Debug for WriteInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("WriteInbox").field(&self.is_set()).finish()
    }
}

/// A new channel: the sender for the app's approver and the inbox for `PluginsStartup::with_writes`.
#[must_use]
pub fn write_channel() -> (WriteSender, WriteInbox) {
    let (tx, rx) = unbounded_channel();
    (tx, WriteInbox(Arc::new(Mutex::new(Some(rx)))))
}

/// An ask that waits for the user.
pub struct PendingWrite {
    /// Tells this ask from another one.
    pub id: u64,
    /// What would run.
    pub request: PluginWriteRequest,
    reply: oneshot::Sender<bool>,
}

impl std::fmt::Debug for PendingWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingWrite")
            .field("id", &self.id)
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

/// The asks that wait for the user, oldest first. The oldest one is shown.
#[derive(Default)]
pub struct WriteQueue {
    waiting: VecDeque<PendingWrite>,
    next_id: u64,
    /// The run gate of the second confirmation.
    gate: QueryTab,
    /// The ask the gate shows the SQL of.
    gated: Option<u64>,
    notice: Option<String>,
}

// Never the SQL (the gate holds it).
impl std::fmt::Debug for WriteQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteQueue")
            .field("waiting", &self.waiting)
            .field("gated", &self.gated)
            .finish_non_exhaustive()
    }
}

impl WriteQueue {
    /// Adds an ask and returns its id. A second confirmation the gate cannot show is denied at once.
    pub fn push(&mut self, ask: PluginWriteAsk) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.waiting.push_back(PendingWrite {
            id,
            request: ask.request,
            reply: ask.reply,
        });
        self.sync_gate();
        id
    }

    /// The ask the user answers now.
    #[must_use]
    pub fn current(&self) -> Option<&PendingWrite> {
        self.waiting.front()
    }

    /// The statements the gate asks to confirm while the current ask is a second confirmation.
    #[must_use]
    pub fn gate_statements(&self) -> Option<&[BoundStatement]> {
        let current = self.current()?;
        if current.request.stage != WriteStage::ProdSecond || self.gated != Some(current.id) {
            return None;
        }
        match self.gate.phase() {
            Phase::Confirming(list) => Some(list),
            _ => None,
        }
    }

    /// Answers ask `id` when it is the current one and at `stage`. Any other id (a card that is gone, an ask that
    /// was answered) does nothing and never answers another ask. A second confirmation is answered only while the
    /// gate shows exactly its SQL. Returns whether it answered.
    pub fn answer(&mut self, id: u64, stage: WriteStage, approve: bool) -> bool {
        let is_it = self
            .current()
            .is_some_and(|p| p.id == id && p.request.stage == stage);
        if !is_it {
            return false;
        }
        if stage == WriteStage::ProdSecond {
            if self.gate_statements().is_none() {
                return false;
            }
            if approve {
                self.gate.confirm();
            } else {
                self.gate.dismiss();
            }
            self.gated = None;
        }
        self.pop(approve);
        self.sync_gate();
        true
    }

    /// Denies the current ask, whichever stage it is at. For the Escape key.
    pub fn deny_current(&mut self) -> bool {
        let Some(current) = self.current() else {
            return false;
        };
        let (id, stage) = (current.id, current.request.stage);
        self.answer(id, stage, false)
    }

    /// Drops the asks nobody waits for any more (the plugin call was cancelled). Returns whether any went.
    pub fn prune_closed(&mut self) -> bool {
        let before = self.waiting.len();
        self.waiting.retain(|p| !p.reply.is_closed());
        let changed = self.waiting.len() != before;
        if changed {
            self.sync_gate();
        }
        changed
    }

    /// Denies every ask that waits.
    pub fn deny_all(&mut self) {
        while self.pop(false) {}
        self.gate.dismiss();
        self.gated = None;
    }

    /// Why the last ask was refused without being shown, once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Whether an ask waits.
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// How many asks wait.
    #[must_use]
    pub fn len(&self) -> usize {
        self.waiting.len()
    }

    /// Whether no ask waits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    fn pop(&mut self, approve: bool) -> bool {
        match self.waiting.pop_front() {
            Some(pending) => {
                // A dropped receiver means the plugin call is gone; nothing waits for the answer.
                let _ = pending.reply.send(approve);
                true
            }
            None => false,
        }
    }

    /// Keeps the gate in step with the front of the queue: open on its SQL when it is a second confirmation, idle
    /// otherwise. An ask whose gate cannot be opened is denied (doubt errs toward refusing).
    fn sync_gate(&mut self) {
        loop {
            let Some(front) = self.waiting.front() else {
                self.gate.dismiss();
                self.gated = None;
                return;
            };
            if front.request.stage != WriteStage::ProdSecond {
                self.gate.dismiss();
                self.gated = None;
                return;
            }
            if self.gated == Some(front.id) {
                return;
            }
            let (id, driver) = (front.id, front.request.driver.clone());
            self.gate.dismiss();
            match open_prod_gate(&mut self.gate, &driver, &front.request.sql) {
                Ok(()) => {
                    self.gated = Some(id);
                    return;
                }
                Err(message) => {
                    self.gated = None;
                    self.notice = Some(message.to_owned());
                    self.pop(false);
                }
            }
        }
    }
}

/// Who asks: the display name (the plugin controls it) and the id (it does not): `Acme Sync (acme-sync)`.
#[must_use]
pub fn asker(request: &PluginWriteRequest) -> String {
    format!("{} ({})", request.plugin_name, request.plugin.as_str())
}

/// The title of the first card: `Acme Sync (acme-sync) wants to run a statement`.
#[must_use]
pub fn card_title(request: &PluginWriteRequest) -> String {
    format!("{} wants to run a statement", asker(request))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "canary-sql-9917";

    fn driver() -> DriverId {
        DriverId::new("sqlite").unwrap()
    }

    fn request(sql: &str, stage: WriteStage, prod: bool) -> PluginWriteRequest {
        PluginWriteRequest::new(
            "acme-sync".parse().unwrap(),
            "Acme Sync",
            "Orders",
            driver(),
            prod,
            sql,
            stage,
        )
    }

    fn ask(sql: &str, stage: WriteStage, prod: bool) -> (PluginWriteAsk, oneshot::Receiver<bool>) {
        let (reply, rx) = oneshot::channel();
        (
            PluginWriteAsk {
                request: request(sql, stage, prod),
                reply,
            },
            rx,
        )
    }

    #[test]
    fn the_queue_keeps_order_and_answers_by_id() {
        let mut queue = WriteQueue::default();
        let (a, mut ra) = ask("update t set a = 1", WriteStage::First, false);
        let (b, mut rb) = ask("delete from t", WriteStage::First, false);
        let (ida, idb) = (queue.push(a), queue.push(b));
        assert_eq!((ida, idb), (0, 1));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.current().unwrap().request.sql, "update t set a = 1");
        // The second id is not the front: nothing happens.
        assert!(!queue.answer(idb, WriteStage::First, true));
        assert_eq!(queue.len(), 2);
        assert!(queue.answer(ida, WriteStage::First, true));
        // The click meant for the first card lands after the queue advanced.
        assert!(!queue.answer(ida, WriteStage::First, true));
        // The wrong stage is a no-op too.
        assert!(!queue.answer(idb, WriteStage::ProdSecond, true));
        assert_eq!(queue.current().unwrap().id, idb);
        assert!(queue.answer(idb, WriteStage::First, false));
        assert!(queue.is_empty());
        assert_eq!(ra.try_recv(), Ok(true));
        assert_eq!(rb.try_recv(), Ok(false));
    }

    #[test]
    fn a_dropped_reply_denies_and_a_dropped_queue_denies() {
        let mut queue = WriteQueue::default();
        let (a, ra) = ask("delete from t", WriteStage::First, false);
        let (b, mut rb) = ask("drop table t", WriteStage::First, false);
        queue.push(a);
        queue.push(b);
        // The plugin call of the first ask was cancelled: its receiver is gone.
        drop(ra);
        assert!(queue.prune_closed());
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.current().unwrap().request.sql, "drop table t");
        // Dropping the queue (window closed) drops the reply: the waiting side sees a closed channel.
        drop(queue);
        assert!(rb.try_recv().is_err());
    }

    #[test]
    fn deny_all_and_escape_deny() {
        let mut queue = WriteQueue::default();
        let (a, mut ra) = ask("update t set a = 1", WriteStage::First, false);
        let (b, mut rb) = ask("delete from t", WriteStage::First, false);
        queue.push(a);
        queue.push(b);
        assert!(queue.deny_current());
        assert_eq!(ra.try_recv(), Ok(false));
        queue.deny_all();
        assert_eq!(rb.try_recv(), Ok(false));
        assert!(!queue.deny_current());
        assert!(queue.is_empty());
    }

    #[test]
    fn a_production_write_is_asked_twice_with_the_exact_sql() {
        let mut queue = WriteQueue::default();
        let sql = format!("update t set a = '{SECRET}' where id = 2");
        let (first, mut r1) = ask(&sql, WriteStage::First, true);
        let (second, mut r2) = ask(&sql, WriteStage::ProdSecond, true);
        let id1 = queue.push(first);
        // The first stage needs no gate.
        assert!(queue.gate_statements().is_none());
        assert!(queue.answer(id1, WriteStage::First, true));
        assert_eq!(r1.try_recv(), Ok(true));
        let id2 = queue.push(second);
        let statements = queue.gate_statements().expect("the gate shows the SQL");
        assert_eq!(statements.len(), 1);
        assert_eq!(statements[0].sql, sql);
        // A click on the wrong stage does nothing; the gate answer approves.
        assert!(!queue.answer(id2, WriteStage::First, true));
        assert!(queue.answer(id2, WriteStage::ProdSecond, true));
        assert_eq!(r2.try_recv(), Ok(true));
        assert!(queue.gate_statements().is_none());
    }

    #[test]
    fn denying_the_second_confirmation_dismisses_the_gate() {
        let mut queue = WriteQueue::default();
        let (second, mut r2) = ask("delete from t", WriteStage::ProdSecond, true);
        let id = queue.push(second);
        assert!(queue.gate_statements().is_some());
        assert!(queue.answer(id, WriteStage::ProdSecond, false));
        assert_eq!(r2.try_recv(), Ok(false));
        assert!(queue.gate_statements().is_none());
    }

    #[test]
    fn a_second_confirmation_the_gate_cannot_show_is_denied() {
        for sql in ["select 1", "update t set a = :x", "", "  "] {
            let mut queue = WriteQueue::default();
            let (a, mut ra) = ask(sql, WriteStage::ProdSecond, true);
            queue.push(a);
            assert!(queue.is_empty(), "{sql}");
            assert_eq!(ra.try_recv(), Ok(false), "{sql}");
            assert!(queue.take_notice().is_some());
            assert!(queue.take_notice().is_none());
        }
    }

    #[test]
    fn a_second_confirmation_behind_another_ask_waits_for_its_turn() {
        let mut queue = WriteQueue::default();
        let (a, _ra) = ask("update t set a = 1", WriteStage::First, true);
        let (b, mut rb) = ask("update t set a = 2", WriteStage::ProdSecond, true);
        let ida = queue.push(a);
        let idb = queue.push(b);
        assert!(queue.gate_statements().is_none());
        assert!(queue.answer(ida, WriteStage::First, true));
        let statements = queue.gate_statements().unwrap();
        assert_eq!(statements[0].sql, "update t set a = 2");
        assert!(queue.answer(idb, WriteStage::ProdSecond, true));
        assert_eq!(rb.try_recv(), Ok(true));
    }

    #[test]
    fn a_closed_channel_is_seen_by_the_sender() {
        let (tx, inbox) = write_channel();
        assert!(inbox.is_set());
        let rx = inbox.take().expect("the receiver");
        assert!(inbox.take().is_none());
        drop(rx);
        let (a, _ra) = ask("delete from t", WriteStage::First, false);
        assert!(tx.send(a).is_err());
    }

    #[test]
    fn debug_never_shows_the_sql() {
        let (a, _ra) = ask(
            &format!("update t set a = '{SECRET}'"),
            WriteStage::First,
            false,
        );
        assert!(!format!("{a:?}").contains(SECRET));
        let mut queue = WriteQueue::default();
        queue.push(a);
        assert!(!format!("{queue:?}").contains(SECRET));
        assert!(!format!("{:?}", queue.current().unwrap()).contains(SECRET));
    }
}
