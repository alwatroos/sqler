// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Approvals of AI statements (ADR-0053), shared by the AI session tab and the agent tab.
//!
//! The tool set asks an [`Approver`] before it runs a statement that is not a plain read. The
//! [`ChannelApprover`] turns each ask into a message on a channel the workspace drains, with a
//! reply sender; the [`ApprovalQueue`] keeps the asks in order until the user answers. Plain Rust:
//! no `GPUI`. A dropped reply, a closed channel or a closed tab always means "deny".
//!
//! The second confirmation on a production connection reuses the run gate of ADR-0024:
//! [`open_prod_gate`] opens it on a [`QueryTab`] with the exact SQL.

use std::cell::Cell;
use std::collections::VecDeque;

use sqler_ai_tools::{ApprovalRequest, ApprovalStage, Approver, Decision};
use sqler_driver_api::BoxFuture;
use sqler_sql::Dialect;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::query::{BufferSnapshot, QueryTab, RunMode, Step, plan};

#[cfg(feature = "gui")]
pub(crate) mod view;

/// Why the production confirmation could not be opened; the statement was not run.
pub const GATE_UNAVAILABLE: &str =
    "This statement cannot be shown in the production confirmation, so it was not run.";

/// One question to the user: the request and the way to answer it.
pub struct ApprovalAsk {
    /// What the tool set wants to run.
    pub request: ApprovalRequest,
    /// Where the answer goes. Dropping it denies.
    pub reply: oneshot::Sender<Decision>,
}

// Never the SQL.
impl std::fmt::Debug for ApprovalAsk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_request(f, "ApprovalAsk", &self.request)
    }
}

fn debug_request(
    f: &mut std::fmt::Formatter<'_>,
    name: &str,
    request: &ApprovalRequest,
) -> std::fmt::Result {
    f.debug_struct(name)
        .field("tool", &request.tool)
        .field("stage", &request.stage)
        .field("index", &request.index)
        .field("count", &request.count)
        .finish_non_exhaustive()
}

/// An [`Approver`] that sends every ask as a message `M` and waits for the reply.
pub struct ChannelApprover<M> {
    tx: UnboundedSender<M>,
    wrap: fn(ApprovalAsk) -> M,
}

impl<M: Send + 'static> ChannelApprover<M> {
    /// An approver that sends `wrap(ask)` on `tx`.
    #[must_use]
    pub fn new(tx: UnboundedSender<M>, wrap: fn(ApprovalAsk) -> M) -> Self {
        Self { tx, wrap }
    }
}

impl<M> std::fmt::Debug for ChannelApprover<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelApprover")
    }
}

impl<M: Send + 'static> Approver for ChannelApprover<M> {
    fn approve<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            let ask = ApprovalAsk {
                request: request.clone(),
                reply,
            };
            if self.tx.send((self.wrap)(ask)).is_err() {
                return Decision::Deny;
            }
            answer.await.unwrap_or(Decision::Deny)
        })
    }
}

/// An ask that waits for the user.
pub struct PendingApproval {
    /// Tells this ask from another one.
    pub id: u64,
    /// What would run.
    pub request: ApprovalRequest,
    reply: oneshot::Sender<Decision>,
}

// Never the SQL.
impl std::fmt::Debug for PendingApproval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_request(f, "PendingApproval", &self.request)
    }
}

/// The asks that wait for the user, oldest first. The oldest one is shown.
#[derive(Debug, Default)]
pub struct ApprovalQueue {
    waiting: VecDeque<PendingApproval>,
    next_id: u64,
    /// The id of the ask whose card the view drew with its exact SQL.
    shown: Cell<Option<u64>>,
    /// The ask the production gate was opened for.
    gate: Option<u64>,
}

impl ApprovalQueue {
    /// Adds an ask and returns its id.
    pub fn push(&mut self, ask: ApprovalAsk) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.waiting.push_back(PendingApproval {
            id,
            request: ask.request,
            reply: ask.reply,
        });
        id
    }

    /// The ask the user answers now.
    #[must_use]
    pub fn current(&self) -> Option<&PendingApproval> {
        self.waiting.front()
    }

    /// Records that the card of ask `id` was drawn with its exact SQL. Called by the view.
    pub fn mark_shown(&self, id: u64) {
        self.shown.set(Some(id));
    }

    /// The id of the current ask when it is at `stage` and the user has seen it; a command that
    /// has no card to click (the palette) may answer only this ask.
    #[must_use]
    pub fn shown_current(&self, stage: ApprovalStage) -> Option<u64> {
        let current = self.current()?;
        (current.request.stage == stage && self.shown.get() == Some(current.id))
            .then_some(current.id)
    }

    /// Records that the production gate shows the SQL of ask `id`.
    pub fn bind_gate(&mut self, id: u64) {
        self.gate = Some(id);
    }

    /// The id of the current ask when it waits at `stage` and the production gate shows exactly
    /// its SQL: the one ask a gate click may answer.
    #[must_use]
    pub fn gated_current(&self) -> Option<u64> {
        let current = self.current()?;
        (current.request.stage == ApprovalStage::ProdSecond && self.gate == Some(current.id))
            .then_some(current.id)
    }

    /// Answers the ask `id` when it is the current one and at `stage`. Any other id (a card that
    /// is gone, an ask that was answered) is a no-op: it never answers another ask. Returns
    /// whether it answered.
    pub fn answer_id(&mut self, id: u64, stage: ApprovalStage, decision: Decision) -> bool {
        let is_it = self
            .current()
            .is_some_and(|p| p.id == id && p.request.stage == stage);
        is_it && self.answer(decision)
    }

    /// Answers the current ask, whichever it is. Private: callers name the ask they answer.
    fn answer(&mut self, decision: Decision) -> bool {
        match self.waiting.pop_front() {
            Some(pending) => {
                // A dropped receiver means the turn is gone; nothing waits for the answer.
                let _ = pending.reply.send(decision);
                true
            }
            None => false,
        }
    }

    /// Denies every ask that waits.
    pub fn deny_all(&mut self) {
        while self.answer(Decision::Deny) {}
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

/// Opens the production confirmation (ADR-0024) on `gate` with exactly `sql`.
///
/// # Errors
/// [`GATE_UNAVAILABLE`] when the statement would not open a confirmation (a read, parameters, an
/// empty plan); the gate is left idle then.
pub fn open_prod_gate(
    gate: &mut QueryTab,
    dialect: Option<Dialect>,
    sql: &str,
) -> Result<(), &'static str> {
    let shown = plan(dialect, &BufferSnapshot::at_cursor(sql, 0), RunMode::All)
        .ok()
        .and_then(|plan| gate.request(dialect, plan, true).ok());
    if matches!(shown, Some(Step::Confirm)) {
        return Ok(());
    }
    gate.dismiss();
    Err(GATE_UNAVAILABLE)
}

/// The title of the approval card: `Approve statement 1 of 2 · write`.
#[must_use]
pub fn card_title(request: &ApprovalRequest) -> String {
    format!(
        "Approve statement {} of {} \u{b7} {}",
        request.index + 1,
        request.count,
        request.reason.as_str()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_ai_api::ToolCall;
    use sqler_ai_tools::{
        ApprovalStage, ConnectionInfo, Limits, SessionSource, ToolError, ToolSet,
    };
    use sqler_catalog::Catalog;
    use sqler_driver_api::sqler_core::{Environment, Query};
    use sqler_driver_api::{CancelFlag, CancelHandle, DriverError, QueryStream, Session};
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::query::Phase;

    const SECRET: &str = "canary-sql-7731";

    /// A session that counts what it is asked to run and fails every statement.
    struct Counting(Arc<AtomicUsize>);

    impl Session for Counting {
        fn cancel_handle(&self) -> CancelHandle {
            CancelFlag::new().handle()
        }

        fn execute<'s>(
            &'s mut self,
            _query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(DriverError::Query("fake".into())) })
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct Source {
        info: ConnectionInfo,
        runs: Arc<AtomicUsize>,
    }

    impl SessionSource for Source {
        fn connections(&self) -> Vec<ConnectionInfo> {
            vec![self.info.clone()]
        }

        fn catalog(&self, _connection: &str) -> Option<Catalog> {
            None
        }

        fn acquire<'a>(
            &'a self,
            _connection: &'a str,
        ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
            let session: Box<dyn Session> = Box::new(Counting(Arc::clone(&self.runs)));
            Box::pin(async move { Ok(session) })
        }
    }

    fn driver() -> sqler_driver_api::sqler_core::DriverId {
        sqler_driver_api::sqler_core::DriverId::new("sqlite").unwrap()
    }

    fn fixture(env: Option<Environment>) -> (Arc<AtomicUsize>, ToolSet) {
        let runs = Arc::new(AtomicUsize::new(0));
        let info = ConnectionInfo::new("c1", "db", driver(), env);
        let source = Arc::new(Source {
            info,
            runs: Arc::clone(&runs),
        });
        (runs, ToolSet::new(source, Limits::default()))
    }

    fn call(sql: &str) -> ToolCall {
        let args = serde_json::json!({"connection": "db", "sql": sql}).to_string();
        ToolCall::new("t1", "run_query", args)
    }

    enum Msg {
        Ask(ApprovalAsk),
    }

    /// Runs an update through a tool set whose approver is a channel; `answer` plays the user.
    async fn run_update(
        answer: impl FnOnce(ApprovalAsk) + Send + 'static,
    ) -> (usize, sqler_ai_api::ToolResult) {
        let (runs, tools) = fixture(None);
        let (tx, mut rx) = unbounded_channel();
        let tools = tools.with_approver(Arc::new(ChannelApprover::new(tx, Msg::Ask)));
        let user = tokio::spawn(async move {
            if let Some(Msg::Ask(ask)) = rx.recv().await {
                answer(ask);
            }
        });
        let sql = format!("update t set a = '{SECRET}'");
        let result = tools.call(&call(&sql)).await;
        user.await.unwrap();
        (runs.load(Ordering::SeqCst), result)
    }

    #[tokio::test]
    async fn approve_runs_the_statement() {
        let (runs, result) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_update(|ask| {
                assert_eq!(ask.request.stage, ApprovalStage::Confirm);
                assert!(ask.request.sql.contains(SECRET));
                ask.reply.send(Decision::Approve).unwrap();
            }),
        )
        .await
        .unwrap();
        assert_eq!(runs, 1);
        assert!(result.content.contains("fake") || result.is_error);
    }

    #[tokio::test]
    async fn deny_refuses_the_statement() {
        let (runs, result) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_update(|ask| {
                ask.reply.send(Decision::Deny).unwrap();
            }),
        )
        .await
        .unwrap();
        assert_eq!(runs, 0);
        assert!(result.is_error);
        assert!(result.content.contains("denied"));
    }

    #[tokio::test]
    async fn a_dropped_reply_denies() {
        let (runs, result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), run_update(drop))
                .await
                .unwrap();
        assert_eq!(runs, 0);
        assert!(result.content.contains("denied"));
    }

    #[tokio::test]
    async fn a_dropped_receiver_denies() {
        let (runs, tools) = fixture(None);
        let (tx, rx) = unbounded_channel::<Msg>();
        drop(rx);
        let tools = tools.with_approver(Arc::new(ChannelApprover::new(tx, Msg::Ask)));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tools.call(&call("delete from t")),
        )
        .await
        .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert!(result.content.contains("denied"));
    }

    #[tokio::test]
    async fn the_queue_keeps_order_and_deny_all_denies() {
        let (_runs, tools) = fixture(None);
        let mut requests = Vec::new();
        for sql in ["update t set a = 1", "delete from t", "drop table t"] {
            let (tx, mut rx) = unbounded_channel();
            let approver = Arc::new(ChannelApprover::new(tx, Msg::Ask));
            let tools = tools.with_approver(approver);
            let c = call(sql);
            let task = tokio::spawn(async move { tools.call(&c).await });
            let Some(Msg::Ask(ask)) = rx.recv().await else {
                panic!("no ask");
            };
            requests.push((ask, task));
        }
        let mut queue = ApprovalQueue::default();
        assert!(!queue.is_waiting());
        assert!(queue.current().is_none());
        assert!(!queue.answer_id(0, ApprovalStage::Confirm, Decision::Approve));
        let mut tasks = Vec::new();
        let mut ids = Vec::new();
        for (ask, task) in requests {
            ids.push(queue.push(ask));
            tasks.push(task);
        }
        assert_eq!(ids, [0, 1, 2]);
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.current().unwrap().id, 0);
        assert_eq!(queue.current().unwrap().request.sql, "update t set a = 1");
        assert!(queue.answer_id(0, ApprovalStage::Confirm, Decision::Deny));
        assert_eq!(queue.current().unwrap().request.sql, "delete from t");
        queue.deny_all();
        assert!(queue.is_empty());
        for task in tasks {
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap();
            assert!(result.content.contains("denied"));
        }
    }

    /// A queue with one ask per statement, and the tasks that wait for the answers.
    async fn queued(
        sqls: &[&str],
    ) -> (
        ApprovalQueue,
        Vec<u64>,
        Vec<tokio::task::JoinHandle<sqler_ai_api::ToolResult>>,
    ) {
        let (_runs, tools) = fixture(None);
        let mut queue = ApprovalQueue::default();
        let (mut ids, mut tasks) = (Vec::new(), Vec::new());
        for sql in sqls {
            let (tx, mut rx) = unbounded_channel();
            let tools = tools.with_approver(Arc::new(ChannelApprover::new(tx, Msg::Ask)));
            let c = call(sql);
            tasks.push(tokio::spawn(async move { tools.call(&c).await }));
            let Some(Msg::Ask(ask)) = rx.recv().await else {
                panic!("no ask");
            };
            ids.push(queue.push(ask));
        }
        (queue, ids, tasks)
    }

    async fn result_of(task: tokio::task::JoinHandle<sqler_ai_api::ToolResult>) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .content
    }

    #[tokio::test]
    async fn a_stale_id_answers_nothing_and_never_the_next_ask() {
        let (mut queue, ids, mut tasks) = queued(&["update t set a = 1", "delete from t"]).await;
        let (first, second) = (ids[0], ids[1]);
        assert!(queue.answer_id(first, ApprovalStage::Confirm, Decision::Deny));
        // The click that was meant for the first card lands after the queue advanced.
        assert!(!queue.answer_id(first, ApprovalStage::Confirm, Decision::Approve));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.current().unwrap().id, second);
        // The wrong stage is a no-op too.
        assert!(!queue.answer_id(second, ApprovalStage::ProdSecond, Decision::Approve));
        assert_eq!(queue.len(), 1);
        assert!(result_of(tasks.remove(0)).await.contains("denied"));
        queue.deny_all();
        assert!(result_of(tasks.remove(0)).await.contains("denied"));
    }

    #[tokio::test]
    async fn two_asks_are_answered_in_order_by_id() {
        let (mut queue, ids, mut tasks) = queued(&["update t set a = 1", "delete from t"]).await;
        // The second id is not the front: nothing happens.
        assert!(!queue.answer_id(ids[1], ApprovalStage::Confirm, Decision::Approve));
        assert_eq!(queue.len(), 2);
        assert!(queue.answer_id(ids[0], ApprovalStage::Confirm, Decision::Approve));
        assert!(queue.answer_id(ids[1], ApprovalStage::Confirm, Decision::Deny));
        assert!(queue.is_empty());
        assert!(!result_of(tasks.remove(0)).await.contains("denied"));
        assert!(result_of(tasks.remove(0)).await.contains("denied"));
    }

    #[tokio::test]
    async fn a_duplicate_answer_to_one_id_is_a_no_op() {
        let (mut queue, ids, mut tasks) = queued(&["update t set a = 1", "delete from t"]).await;
        assert!(queue.answer_id(ids[0], ApprovalStage::Confirm, Decision::Deny));
        assert!(!queue.answer_id(ids[0], ApprovalStage::Confirm, Decision::Approve));
        assert_eq!(queue.current().unwrap().id, ids[1]);
        queue.deny_all();
        for task in tasks.drain(..) {
            assert!(result_of(task).await.contains("denied"));
        }
    }

    #[tokio::test]
    async fn a_command_answers_only_an_ask_that_was_shown() {
        let (mut queue, ids, tasks) = queued(&["update t set a = 1", "delete from t"]).await;
        assert_eq!(queue.shown_current(ApprovalStage::Confirm), None);
        queue.mark_shown(ids[0]);
        assert_eq!(queue.shown_current(ApprovalStage::Confirm), Some(ids[0]));
        assert_eq!(queue.shown_current(ApprovalStage::ProdSecond), None);
        assert!(queue.answer_id(ids[0], ApprovalStage::Confirm, Decision::Deny));
        // The second ask became the front but its card was not drawn yet.
        assert_eq!(queue.shown_current(ApprovalStage::Confirm), None);
        queue.mark_shown(ids[1]);
        assert_eq!(queue.shown_current(ApprovalStage::Confirm), Some(ids[1]));
        queue.deny_all();
        for task in tasks {
            assert!(result_of(task).await.contains("denied"));
        }
    }

    #[tokio::test]
    async fn a_gate_click_answers_only_the_ask_the_gate_shows() {
        let (mut queue, ids, tasks) = queued(&["update t set a = 1"]).await;
        assert_eq!(queue.gated_current(), None);
        queue.bind_gate(ids[0] + 7);
        assert_eq!(queue.gated_current(), None);
        queue.deny_all();
        for task in tasks {
            result_of(task).await;
        }
    }

    #[test]
    fn the_prod_gate_shows_the_exact_sql() {
        let mut gate = QueryTab::new();
        let sql = "update t set a = 1 where id = 2";
        open_prod_gate(&mut gate, Dialect::from_driver_id("sqlite"), sql).unwrap();
        let Phase::Confirming(list) = gate.phase() else {
            panic!("not confirming: {:?}", gate.phase());
        };
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].sql, sql);
    }

    #[test]
    fn the_prod_gate_refuses_what_it_cannot_confirm() {
        for sql in ["select 1", "update t set a = :x", "", "   "] {
            let mut gate = QueryTab::new();
            assert_eq!(
                open_prod_gate(&mut gate, Dialect::from_driver_id("sqlite"), sql),
                Err(GATE_UNAVAILABLE),
                "{sql}"
            );
            assert_eq!(gate.phase(), &Phase::Idle, "{sql}");
        }
    }

    #[test]
    fn the_card_title_counts_from_one() {
        let request = request_for("update t set a = 1");
        assert_eq!(
            card_title(&request),
            "Approve statement 1 of 1 \u{b7} write"
        );
    }

    /// The request the tool set builds for `sql`.
    fn request_for(sql: &str) -> ApprovalRequest {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (_runs, tools) = fixture(None);
            let (tx, mut rx) = unbounded_channel();
            let tools = tools.with_approver(Arc::new(ChannelApprover::new(tx, Msg::Ask)));
            let c = call(sql);
            let task = tokio::spawn(async move { tools.call(&c).await });
            let Some(Msg::Ask(ask)) = rx.recv().await else {
                panic!("no ask");
            };
            let request = ask.request.clone();
            drop(ask);
            task.await.unwrap();
            request
        })
    }

    #[test]
    fn debug_never_shows_the_sql() {
        let request = request_for(&format!("update t set a = '{SECRET}'"));
        let (reply, _rx) = oneshot::channel();
        let ask = ApprovalAsk {
            request: request.clone(),
            reply,
        };
        assert!(!format!("{ask:?}").contains(SECRET));
        assert!(format!("{ask:?}").contains("run_query"));
        let mut queue = ApprovalQueue::default();
        queue.push(ask);
        assert!(!format!("{queue:?}").contains(SECRET));
        assert!(!format!("{:?}", queue.current().unwrap()).contains(SECRET));
    }
}
