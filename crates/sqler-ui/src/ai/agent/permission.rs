// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The permission requests of an agent that wait for the user (ADR-0051).
//!
//! `ApprovalRequest` of the AI tools is `#[non_exhaustive]` and has no constructor, so the
//! `ApprovalQueue` (ADR-0053) cannot hold an ACP request. This queue keeps the same contract for
//! it: an ask has an id, only the front ask can be answered, an answer names the ask and an
//! option the agent offered, a command may answer only an ask whose card was drawn, and a dropped
//! queue or reply cancels the request. The tests mirror the approval tests one by one so the two
//! stay provably equivalent.
//!
//! The agent's text is bounded the way the transcript bounds it: the title, the kind, every option
//! name and the text lines are cut to [`MAX_ENTRY_BYTES`] (the lines share one budget), and option
//! ids are never cut, because an answer names them. At most [`MAX_WAITING`] asks wait; the next
//! one is answered `Cancelled` at once. Follow-up: make the approval queue generic and fold this one in.

use std::cell::Cell;
use std::collections::VecDeque;

use sqler_acp::{PermissionDecision, PermissionRequest};
use tokio::sync::oneshot;

use super::host::PermissionAsk;
use super::transcript::{MAX_ENTRY_BYTES, cut_str};

/// The most asks that wait at once. A further ask is cancelled when it arrives.
pub const MAX_WAITING: usize = 8;

/// An ask that waits for the user.
pub struct PendingPermission {
    /// Tells this ask from another one.
    pub id: u64,
    /// What the agent wants to do and the options it offers, in the order it sent them.
    pub request: PermissionRequest,
    reply: oneshot::Sender<PermissionDecision>,
}

// Never the tool call: its title and text come from the agent.
impl std::fmt::Debug for PendingPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingPermission")
            .field("id", &self.id)
            .field("options", &self.request.options.len())
            .finish_non_exhaustive()
    }
}

/// Cuts the texts of the agent in `request` to [`MAX_ENTRY_BYTES`]. Option ids stay whole.
fn cap_request(request: &mut PermissionRequest) {
    let call = &mut request.tool_call;
    if let Some(title) = &mut call.title {
        title.truncate(cut_str(title, MAX_ENTRY_BYTES).len());
    }
    if let Some(kind) = &mut call.kind {
        kind.truncate(cut_str(kind, MAX_ENTRY_BYTES).len());
    }
    let mut room = MAX_ENTRY_BYTES;
    for line in &mut call.text {
        let keep = cut_str(line, room).len();
        line.truncate(keep);
        room -= keep;
    }
    for option in &mut request.options {
        option
            .name
            .truncate(cut_str(&option.name, MAX_ENTRY_BYTES).len());
    }
}

/// The permission requests that wait for the user, oldest first. The oldest one is shown.
#[derive(Debug, Default)]
pub struct PermissionQueue {
    waiting: VecDeque<PendingPermission>,
    next_id: u64,
    /// The id of the ask whose card the view drew.
    shown: Cell<Option<u64>>,
}

impl PermissionQueue {
    /// Adds an ask and returns its id. Its texts are cut to [`MAX_ENTRY_BYTES`]. When
    /// `MAX_WAITING` asks wait already, the ask is answered `Cancelled` and `None` is returned.
    pub fn push(&mut self, ask: PermissionAsk) -> Option<u64> {
        if self.waiting.len() >= MAX_WAITING {
            // A dropped receiver means the agent is gone; nothing waits for the answer.
            let _ = ask.reply.send(PermissionDecision::Cancelled);
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut request = ask.request;
        cap_request(&mut request);
        self.waiting.push_back(PendingPermission {
            id,
            request,
            reply: ask.reply,
        });
        Some(id)
    }

    /// The ask the user answers now.
    #[must_use]
    pub fn current(&self) -> Option<&PendingPermission> {
        self.waiting.front()
    }

    /// Records that the card of ask `id` was drawn. Called by the view.
    pub fn mark_shown(&self, id: u64) {
        self.shown.set(Some(id));
    }

    /// The id of the current ask when the user has seen its card; a command that has no card to
    /// click (the palette) may answer only this ask.
    #[must_use]
    pub fn shown_current(&self) -> Option<u64> {
        let current = self.current()?;
        (self.shown.get() == Some(current.id)).then_some(current.id)
    }

    /// Answers ask `id` with the option `option` when it is the current one and the agent offered
    /// that option. Anything else (a card that is gone, an ask that was answered, an option that
    /// was not offered) is a no-op: it never answers another ask. Returns whether it answered.
    pub fn answer_id(&mut self, id: u64, option: &str) -> bool {
        let fits = self
            .current()
            .is_some_and(|p| p.id == id && p.request.options.iter().any(|o| o.id == option));
        fits && self.answer(PermissionDecision::Selected(option.to_owned()))
    }

    /// Answers the current ask, whichever it is. Private: callers name the ask they answer.
    fn answer(&mut self, decision: PermissionDecision) -> bool {
        match self.waiting.pop_front() {
            Some(pending) => {
                // A dropped receiver means the agent is gone; nothing waits for the answer.
                let _ = pending.reply.send(decision);
                true
            }
            None => false,
        }
    }

    /// Cancels every ask that waits.
    pub fn cancel_all(&mut self) {
        while self.answer(PermissionDecision::Cancelled) {}
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
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqler_acp::{AcpHost, PermissionOption, PermissionOptionKind, SessionId, ToolCallInfo};
    use tokio::sync::mpsc::unbounded_channel;

    use super::super::host::{AgentMsg, TabAcpHost};
    use super::*;

    const SECRET: &str = "canary-title-5521";

    fn request(title: &str) -> PermissionRequest {
        PermissionRequest {
            session_id: SessionId::new("s"),
            tool_call: ToolCallInfo {
                id: "call".to_owned(),
                title: Some(title.to_owned()),
                kind: None,
                status: None,
                text: Vec::new(),
            },
            options: vec![
                PermissionOption {
                    id: "allow-once".to_owned(),
                    name: "Allow once".to_owned(),
                    kind: PermissionOptionKind::AllowOnce,
                },
                PermissionOption {
                    id: "reject-once".to_owned(),
                    name: "Reject once".to_owned(),
                    kind: PermissionOptionKind::RejectOnce,
                },
            ],
        }
    }

    type Waiting = tokio::task::JoinHandle<PermissionDecision>;

    /// One request through a `TabAcpHost`, as the agent makes it: the ask and the task that
    /// waits for the answer.
    async fn make_ask(title: &str) -> (PermissionAsk, Waiting) {
        let (tx, mut rx) = unbounded_channel();
        let host = TabAcpHost(tx);
        let req = request(title);
        let waiting = tokio::spawn(async move { host.request_permission(req).await });
        let Some(AgentMsg::Permission(ask)) = rx.recv().await else {
            panic!("no ask");
        };
        (ask, waiting)
    }

    async fn queued(titles: &[&str]) -> (PermissionQueue, Vec<u64>, Vec<Waiting>) {
        let mut queue = PermissionQueue::default();
        let (mut ids, mut tasks) = (Vec::new(), Vec::new());
        for title in titles {
            let (ask, task) = make_ask(title).await;
            ids.push(queue.push(ask).unwrap());
            tasks.push(task);
        }
        (queue, ids, tasks)
    }

    async fn decision(task: Waiting) -> PermissionDecision {
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
    }

    fn selected(id: &str) -> PermissionDecision {
        PermissionDecision::Selected(id.to_owned())
    }

    #[tokio::test]
    async fn the_queue_keeps_order_and_cancel_all_cancels() {
        let mut empty = PermissionQueue::default();
        assert!(!empty.is_waiting());
        assert!(empty.current().is_none());
        assert!(!empty.answer_id(0, "allow-once"));
        let (mut queue, ids, mut tasks) = queued(&["one", "two", "three"]).await;
        assert_eq!(ids, [0, 1, 2]);
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.current().unwrap().id, 0);
        assert_eq!(
            queue.current().unwrap().request.tool_call.title.as_deref(),
            Some("one")
        );
        assert!(queue.answer_id(0, "reject-once"));
        assert_eq!(
            queue.current().unwrap().request.tool_call.title.as_deref(),
            Some("two")
        );
        queue.cancel_all();
        assert!(queue.is_empty());
        assert_eq!(decision(tasks.remove(0)).await, selected("reject-once"));
        for task in tasks {
            assert_eq!(decision(task).await, PermissionDecision::Cancelled);
        }
    }

    #[tokio::test]
    async fn the_ninth_ask_is_cancelled_and_not_queued() {
        let mut queue = PermissionQueue::default();
        let mut tasks = Vec::new();
        for i in 0..MAX_WAITING {
            let (ask, task) = make_ask(&format!("ask {i}")).await;
            assert_eq!(queue.push(ask), Some(i as u64));
            tasks.push(task);
        }
        let (ask, task) = make_ask("one too many").await;
        assert_eq!(queue.push(ask), None);
        assert_eq!(decision(task).await, PermissionDecision::Cancelled);
        assert_eq!(queue.len(), MAX_WAITING);
        // Room again after an answer.
        assert!(queue.answer_id(0, "reject-once"));
        let (ask, _task) = make_ask("fits now").await;
        assert_eq!(queue.push(ask), Some(MAX_WAITING as u64));
        queue.cancel_all();
    }

    #[tokio::test]
    async fn oversize_texts_are_cut_on_char_boundaries_and_ids_are_kept() {
        // Two-byte characters, so an odd limit cuts inside one.
        let big = "é".repeat(MAX_ENTRY_BYTES);
        let (tx, mut rx) = unbounded_channel();
        let host = TabAcpHost(tx);
        let mut req = request(&big);
        req.tool_call.kind = Some(big.clone());
        req.tool_call.text = vec![big.clone(), big.clone(), "tail".to_owned()];
        req.options[0].name = big.clone();
        let long_id = "i".repeat(MAX_ENTRY_BYTES + 5);
        req.options[1].id.clone_from(&long_id);
        let _waiting = tokio::spawn(async move { host.request_permission(req).await });
        let Some(AgentMsg::Permission(ask)) = rx.recv().await else {
            panic!("no ask");
        };
        let mut queue = PermissionQueue::default();
        queue.push(ask).unwrap();
        let r = &queue.current().unwrap().request;
        let call = &r.tool_call;
        assert!(call.title.as_ref().unwrap().len() <= MAX_ENTRY_BYTES);
        assert!(call.kind.as_ref().unwrap().len() <= MAX_ENTRY_BYTES);
        assert!(call.text.iter().map(String::len).sum::<usize>() <= MAX_ENTRY_BYTES);
        assert_eq!(call.text.len(), 3);
        assert!(r.options[0].name.len() <= MAX_ENTRY_BYTES);
        assert!(r.options[0].name.chars().all(|c| c == 'é'));
        assert_eq!(r.options[0].id, "allow-once");
        assert_eq!(r.options[1].id, long_id);
        queue.cancel_all();
    }

    #[tokio::test]
    async fn a_stale_id_answers_nothing_and_never_the_next_ask() {
        let (mut queue, ids, mut tasks) = queued(&["one", "two"]).await;
        let (first, second) = (ids[0], ids[1]);
        assert!(queue.answer_id(first, "reject-once"));
        // The click that was meant for the first card lands after the queue advanced.
        assert!(!queue.answer_id(first, "allow-once"));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.current().unwrap().id, second);
        // An option the agent did not offer is a no-op too (the counterpart of a wrong stage).
        assert!(!queue.answer_id(second, "allow-always"));
        assert_eq!(queue.len(), 1);
        assert_eq!(decision(tasks.remove(0)).await, selected("reject-once"));
        queue.cancel_all();
        assert_eq!(
            decision(tasks.remove(0)).await,
            PermissionDecision::Cancelled
        );
    }

    #[tokio::test]
    async fn two_asks_are_answered_in_order_by_id() {
        let (mut queue, ids, mut tasks) = queued(&["one", "two"]).await;
        // The second id is not the front: nothing happens.
        assert!(!queue.answer_id(ids[1], "allow-once"));
        assert_eq!(queue.len(), 2);
        assert!(queue.answer_id(ids[0], "allow-once"));
        assert!(queue.answer_id(ids[1], "reject-once"));
        assert!(queue.is_empty());
        assert_eq!(decision(tasks.remove(0)).await, selected("allow-once"));
        assert_eq!(decision(tasks.remove(0)).await, selected("reject-once"));
    }

    #[tokio::test]
    async fn a_duplicate_answer_to_one_id_is_a_no_op() {
        let (mut queue, ids, tasks) = queued(&["one", "two"]).await;
        assert!(queue.answer_id(ids[0], "reject-once"));
        assert!(!queue.answer_id(ids[0], "allow-once"));
        assert_eq!(queue.current().unwrap().id, ids[1]);
        queue.cancel_all();
        let mut results = Vec::new();
        for task in tasks {
            results.push(decision(task).await);
        }
        assert_eq!(
            results,
            [selected("reject-once"), PermissionDecision::Cancelled]
        );
    }

    #[tokio::test]
    async fn a_command_answers_only_an_ask_that_was_shown() {
        let (mut queue, ids, tasks) = queued(&["one", "two"]).await;
        assert_eq!(queue.shown_current(), None);
        queue.mark_shown(ids[0]);
        assert_eq!(queue.shown_current(), Some(ids[0]));
        assert!(queue.answer_id(ids[0], "reject-once"));
        // The second ask became the front but its card was not drawn yet.
        assert_eq!(queue.shown_current(), None);
        queue.mark_shown(ids[1]);
        assert_eq!(queue.shown_current(), Some(ids[1]));
        queue.cancel_all();
        for task in tasks {
            decision(task).await;
        }
    }

    #[tokio::test]
    async fn a_dropped_queue_or_reply_cancels_and_a_closed_channel_cancels() {
        // The queue is dropped with an ask in it.
        let (queue, _ids, mut tasks) = queued(&["one"]).await;
        drop(queue);
        assert_eq!(
            decision(tasks.remove(0)).await,
            PermissionDecision::Cancelled
        );
        // The ask is dropped without an answer.
        let (ask, task) = make_ask("two").await;
        drop(ask);
        assert_eq!(decision(task).await, PermissionDecision::Cancelled);
        // The receiver is gone.
        let (tx, rx) = unbounded_channel::<AgentMsg>();
        drop(rx);
        let answer = TabAcpHost(tx).request_permission(request("three")).await;
        assert_eq!(answer, PermissionDecision::Cancelled);
    }

    #[tokio::test]
    async fn debug_never_shows_the_tool_call() {
        let (ask, _task) = make_ask(SECRET).await;
        assert!(!format!("{ask:?}").contains(SECRET));
        assert!(
            !format!(
                "{:?}",
                AgentMsg::Permission(PermissionAsk {
                    request: request(SECRET),
                    reply: oneshot::channel().0,
                })
            )
            .contains(SECRET)
        );
        let mut queue = PermissionQueue::default();
        queue.push(ask).unwrap();
        assert!(!format!("{queue:?}").contains(SECRET));
        assert!(!format!("{:?}", queue.current().unwrap()).contains(SECRET));
    }
}
