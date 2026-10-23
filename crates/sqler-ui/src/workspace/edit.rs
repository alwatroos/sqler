// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Inline editing in the workspace: the apply flow of ADR-0033.
//!
//! This is the data-modifying path, so it has one gate. Every way to apply (the Apply button, the
//! `grid:apply-changes` chord, the palette, the preview dialog) calls
//! [`Workspace::request_apply`] with a plan built for that attempt. That call
//!
//! 1. refuses, with a notice and no effect, when the tab is gone, a query runs or prompts on it,
//!    an apply is in flight, the session may be inside the user's transaction, a previous
//!    `COMMIT` has an unknown outcome, or the plan could not be built;
//! 2. derives `production` from the connection of the tab (never from what the grid was told),
//!    failing closed;
//! 3. starts the job at once when no confirmation is needed, or stores one [`EditConfirm`] that
//!    owns the plan and lists every statement.
//!
//! [`Workspace::confirm_apply`] takes that confirmation (so it can be used once), derives
//! `production` again, and is the only place `user_confirmed` becomes true.
//!
//! The session an apply runs on is the query tab's own, lent for the apply like a run lends it,
//! for a query result; a table-data tab has none and the job opens a short-lived one.
//!
//! Nothing here formats a plan, a statement or a value with `{:?}`.

use std::collections::{HashMap, HashSet};

use sqler_catalog::CacheKey;
use sqler_config::ConnectionId;
use sqler_driver_api::CancelHandle;
use sqler_driver_api::sqler_core::{DbObject, Environment, ObjectKind, ObjectLevel, ObjectPath};
use sqler_sql::{Dialect, SqlIdent, single_table_source};

use super::state::{Effect, Workspace};
use super::tx::{TxEffect, tx_effect};
use crate::grid::edit::{
    ApplyFailure, ApplyId, ApplyJob, ApplyJobDone, ApplyOutcome, ApplyPlan, ApplySession,
    ApplyStop, COMMIT_UNKNOWN, EditContext, EditError, EditOrigin, GridKey, LAST_WRITER_WINS,
    PreviewStatement, preview_text,
};
use crate::query::{Phase, RunEvent, RunId, StatementStatus};
use crate::tabs::TabId;

const BUSY_APPLYING: &str = "Changes are being applied on this tab.";
const BUSY_RUNNING: &str = "A query is running on this tab.";
const BUSY_TRANSACTION: &str = "This tab may be inside a transaction. Discard the changes, run COMMIT or ROLLBACK (with autocommit off, SET autocommit = 1), then edit again.";
const CHANGED_SINCE_PREVIEW: &str =
    "The changes were edited after the confirmation was shown. Nothing was written; apply again.";
const NO_SESSION: &str = "The connection of this tab is not available.";
const NO_SOURCE: &str = "This grid has no source table.";

/// What a pushed edit context was built from; see `Workspace::edit_push_stale`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditPushState {
    generation: u64,
    apply_block: Option<String>,
}

/// An apply in flight on a tab.
pub(super) struct ActiveApply {
    run: ApplyId,
    stop: ApplyStop,
}

/// What the preview dialog shows: the statements of the plan with their values, and the text to
/// copy. It owns no plan; Apply from the preview asks the grid for a new one.
#[derive(Clone, PartialEq)]
pub struct EditPreview {
    /// The grid.
    pub key: GridKey,
    /// The statements in order, with their bound values.
    pub statements: Vec<PreviewStatement>,
    /// The same as plain text (SQL, values, and the last-writer-wins notice), for Copy.
    pub text: String,
}

impl std::fmt::Debug for EditPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditPreview")
            .field("key", &self.key)
            .field("statements", &self.statements.len())
            .finish_non_exhaustive()
    }
}

/// The production confirmation: every statement of the plan, which it owns.
///
/// [`Workspace::confirm_apply`] consumes it; a plan is confirmed once.
pub struct EditConfirm {
    /// The grid.
    pub key: GridKey,
    /// The name of the connection that is about to be written to.
    pub connection: String,
    /// The statements in order, with their bound values: what will run.
    pub statements: Vec<PreviewStatement>,
    /// The same as plain text, with the last-writer-wins notice.
    pub text: String,
    plan: ApplyPlan,
}

impl EditConfirm {
    /// The notice the confirmation shows with the statements.
    #[must_use]
    pub fn notice(&self) -> &'static str {
        LAST_WRITER_WINS
    }

    /// The number of data statements (without `BEGIN`, `COMMIT` and `ROLLBACK`).
    #[must_use]
    pub fn data_statements(&self) -> usize {
        self.plan.statements().len()
    }
}

impl std::fmt::Debug for EditConfirm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditConfirm")
            .field("key", &self.key)
            .field("statements", &self.statements.len())
            .finish_non_exhaustive()
    }
}

/// What a close prompt is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTarget {
    /// Closing this tab.
    Tab(TabId),
    /// Quitting the application.
    Quit,
}

/// The question "discard the pending changes?" before a tab closes or the app quits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosePrompt {
    /// What would go away.
    pub target: CloseTarget,
    /// The number of pending changes that would be lost.
    pub pending: usize,
    /// The tab's editor also holds unsaved text that would be lost (batch close only).
    pub unsaved_text: bool,
}

/// The result of [`Workspace::apply_finished`].
#[derive(Debug)]
pub struct ApplyFinished {
    /// What the view still has to do (close a session whose tab is gone).
    pub effect: Effect,
    /// The grid the apply was for; give `outcome` to its `finish_apply`.
    pub key: GridKey,
    /// How the apply ended.
    pub outcome: ApplyOutcome,
}

/// The outcome to give a grid's `finish_apply` when the apply job ended without reporting (see
/// [`Workspace::apply_lost`]): the server may or may not have committed.
#[must_use]
pub fn lost_apply_outcome() -> ApplyOutcome {
    ApplyOutcome::CommitUnknown {
        failure: ApplyFailure::Driver("the apply ended unexpectedly".to_owned()),
        rollback_error: None,
    }
}

/// The editing state of the workspace. One field of [`Workspace`].
#[derive(Default)]
pub(super) struct EditFlow {
    /// Counts changes of anything an [`EditContext`] depends on.
    generation: u64,
    /// Pending changes per grid, as the view reports them.
    pending: HashMap<GridKey, usize>,
    /// The object each table-data tab shows.
    pub(super) data_tabs: HashMap<TabId, DbObject>,
    /// The schema the server resolved a result's table name to, per `(tab, position)`.
    resolved: HashMap<(TabId, usize), Option<String>>,
    /// Query tabs whose session may be inside a user transaction.
    in_tx: HashSet<TabId>,
    /// T-SQL: the `@@TRANCOUNT` the statements of the tab left, as far as the text shows.
    tran_depth: HashMap<TabId, u32>,
    /// Query tabs whose session has `autocommit = 0`: ending a transaction starts the next one.
    autocommit_off: HashSet<TabId>,
    /// Applies in flight.
    applies: HashMap<TabId, ActiveApply>,
    /// The last apply id handed out.
    next_apply: u64,
    /// Grids whose last `COMMIT` has an unknown outcome; cleared by discarding.
    commit_unknown: HashSet<GridKey>,
    /// Table loads already asked for, per browser generation.
    table_requests: HashSet<(ConnectionId, u64, DbObject)>,
    preview: Option<EditPreview>,
    confirm: Option<EditConfirm>,
    close_prompt: Option<ClosePrompt>,
}

impl EditFlow {
    pub(super) fn bump(&mut self) {
        self.generation += 1;
    }

    fn pending_for_tab(&self, tab: TabId) -> usize {
        self.pending
            .iter()
            .filter(|(key, _)| key.tab == tab)
            .map(|(_, n)| *n)
            .sum()
    }

    fn pending_total(&self) -> usize {
        self.pending.values().sum()
    }

    /// Drops everything kept for the grids of a tab that is going away.
    pub(super) fn forget_tab(&mut self, tab: TabId) {
        self.pending.retain(|key, _| key.tab != tab);
        self.data_tabs.remove(&tab);
        self.resolved.retain(|(t, _), _| *t != tab);
        self.in_tx.remove(&tab);
        self.tran_depth.remove(&tab);
        self.autocommit_off.remove(&tab);
        self.applies.remove(&tab);
        self.commit_unknown.retain(|key| key.tab != tab);
        self.preview = self.preview.take().filter(|p| p.key.tab != tab);
        self.confirm = self.confirm.take().filter(|c| c.key.tab != tab);
        self.close_prompt = self
            .close_prompt
            .take()
            .filter(|p| p.target != CloseTarget::Tab(tab));
        self.generation += 1;
    }

    /// Applies the effect of one statement on the transaction state of `tab`; whether anything
    /// changed. `sent`: the statement went out, whatever became of it; `ended`: it ran to an
    /// answer (success or error); `finished`: it succeeded.
    pub(super) fn follow(
        &mut self,
        tab: TabId,
        effect: TxEffect,
        sent: bool,
        ended: bool,
        finished: bool,
    ) -> bool {
        match effect {
            // A begin counts as soon as the statement was sent, whatever became of it.
            TxEffect::Begin if sent => self.in_tx.insert(tab),
            // An end counts when the statement ran to an answer: `SQLite` fails a `COMMIT`
            // outside a transaction, and the user must not be stuck. With `autocommit = 0` the
            // next transaction starts at once, so the flag stays.
            TxEffect::End if ended => {
                !self.autocommit_off.contains(&tab) && self.in_tx.remove(&tab)
            }
            // T-SQL counts nested `BEGIN TRAN`s like `@@TRANCOUNT`: a `COMMIT` ends the
            // transaction only at the outermost level, a bare `ROLLBACK` always.
            // A change of nothing (`BEGIN TRAN; COMMIT`) leaves the state as it is: when unsure,
            // the transaction stays open.
            TxEffect::Tran {
                reset: false,
                delta: 0,
                floor: 0,
            } => false,
            TxEffect::Tran {
                reset,
                delta,
                floor,
            } if ((delta > 0 || floor > 0) && sent) || (delta <= 0 && ended) => {
                let depth = self.tran_depth.get(&tab).copied().unwrap_or(0);
                let base = if reset { 0 } else { depth };
                // The count is clamped at zero at every statement: `floor` is what that leaves.
                let next = base.saturating_add_signed(delta).max(floor);
                if next > 0 {
                    self.tran_depth.insert(tab, next);
                    self.in_tx.insert(tab) || next != depth
                } else {
                    self.tran_depth.remove(&tab);
                    !self.autocommit_off.contains(&tab) && self.in_tx.remove(&tab)
                }
            }
            TxEffect::AutocommitOff if sent => {
                let first = self.autocommit_off.insert(tab);
                self.in_tx.insert(tab) || first
            }
            // The server commits an open transaction when autocommit is switched on.
            TxEffect::AutocommitOn if finished => {
                let off = self.autocommit_off.remove(&tab);
                self.tran_depth.remove(&tab);
                self.in_tx.remove(&tab) || off
            }
            _ => false,
        }
    }

    /// The tab runs a new query: its old grids, and what was known about them, are gone.
    pub(super) fn run_started(&mut self, tab: TabId) {
        self.pending.retain(|key, _| key.tab != tab);
        self.resolved.retain(|(t, _), _| *t != tab);
        self.commit_unknown.retain(|key| key.tab != tab);
        self.preview = self.preview.take().filter(|p| p.key.tab != tab);
        self.confirm = self.confirm.take().filter(|c| c.key.tab != tab);
        self.generation += 1;
    }

    /// The session of the tab is gone or lost: it is in no transaction any more.
    pub(super) fn session_gone(&mut self, tab: TabId) {
        let off = self.autocommit_off.remove(&tab);
        self.tran_depth.remove(&tab);
        if self.in_tx.remove(&tab) || off {
            self.generation += 1;
        }
    }

    /// Raises the stop switch of every apply; returns the handles to ask the databases with.
    pub(super) fn stop_all_applies(&mut self) -> Vec<CancelHandle> {
        self.applies
            .drain()
            .filter_map(|(_, active)| active.stop.cancel())
            .collect()
    }

    pub(super) fn is_applying(&self, tab: TabId) -> bool {
        self.applies.contains_key(&tab)
    }

    /// Marks `tab` as applying, for tests of the guards around a running apply.
    #[cfg(test)]
    pub(super) fn mark_applying(&mut self, tab: TabId) {
        self.applies.insert(
            tab,
            ActiveApply {
                run: ApplyId::new(1),
                stop: ApplyStop::new(),
            },
        );
    }

    pub(super) fn pending_notice(&self, tab: TabId) -> Option<String> {
        let n = self.pending_for_tab(tab);
        (n > 0).then(|| format!("Apply or discard the {n} pending change(s) first."))
    }

    pub(super) fn quit_prompt(&mut self) -> Option<ClosePrompt> {
        let pending = self.pending_total();
        if pending == 0 {
            return None;
        }
        let prompt = ClosePrompt {
            target: CloseTarget::Quit,
            pending,
            unsaved_text: false,
        };
        self.close_prompt = Some(prompt);
        Some(prompt)
    }

    pub(super) fn tab_prompt(&mut self, tab: TabId) -> Option<ClosePrompt> {
        let pending = self.pending_for_tab(tab);
        if pending == 0 {
            return None;
        }
        let prompt = ClosePrompt {
            target: CloseTarget::Tab(tab),
            pending,
            unsaved_text: false,
        };
        self.close_prompt = Some(prompt);
        Some(prompt)
    }

    /// Asks before a batch close discards the unsaved editor text of `tab`.
    pub(super) fn text_prompt(&mut self, tab: TabId) -> ClosePrompt {
        let prompt = ClosePrompt {
            target: CloseTarget::Tab(tab),
            pending: self.pending_for_tab(tab),
            unsaved_text: true,
        };
        self.close_prompt = Some(prompt);
        prompt
    }
}

/// Whether an environment label means "production" for the apply gate.
///
/// Only the labels known not to be production (`Dev`, `Staging`) are exempt. A label this code
/// does not know (the enum is open) counts as production: the gate fails closed.
fn environment_is_production(environment: Environment) -> bool {
    !matches!(environment, Environment::Dev | Environment::Staging)
}

/// The statement position of an event.
fn event_index(event: &RunEvent) -> usize {
    match event {
        RunEvent::Started { index }
        | RunEvent::Finished { index, .. }
        | RunEvent::Failed { index, .. }
        | RunEvent::Cancelled { index }
        | RunEvent::Resolved { index, .. } => *index,
    }
}

/// The schema (`MySQL`: schema or database) a catalog object lives in.
fn qualifier(dialect: Dialect, object: &DbObject) -> Option<&str> {
    if object.kind.level() != ObjectLevel::Schema {
        return None;
    }
    match dialect {
        Dialect::MySql => object
            .parent
            .schema
            .as_deref()
            .or(object.parent.database.as_deref()),
        _ => object.parent.schema.as_deref(),
    }
}

/// The catalog relation a query's table most likely is: among the relations of that name, the
/// one in the schema the query names or the server resolved; else the first. A wrong pick is
/// harmless: `check_editable` compares the schemas again and refuses a mismatch.
fn pick_relation(
    dialect: Dialect,
    name: &SqlIdent,
    schema: Option<&SqlIdent>,
    resolved: Option<&str>,
    candidates: Vec<DbObject>,
) -> Option<DbObject> {
    let named: Vec<DbObject> = candidates
        .into_iter()
        .filter(|o| name.matches(dialect, &o.name))
        .collect();
    let in_schema = |o: &DbObject| match (schema, qualifier(dialect, o)) {
        (Some(s), Some(q)) => s.matches(dialect, q),
        (None, Some(q)) => resolved == Some(q),
        _ => false,
    };
    named
        .iter()
        .find(|o| in_schema(o))
        .or_else(|| named.first())
        .cloned()
}

impl Workspace {
    // ---- reading ----

    /// Counts changes of anything an [`EditContext`] depends on; push a new context when it
    /// changed.
    #[must_use]
    pub fn edit_generation(&self) -> u64 {
        self.edit.generation
    }

    /// What a context pushed now depends on: the generation and the apply gate, which changes
    /// with the run phase and the session without bumping the generation.
    #[must_use]
    pub fn edit_push_state(&self, key: GridKey) -> EditPushState {
        EditPushState {
            generation: self.edit.generation,
            apply_block: self.apply_block(key),
        }
    }

    /// Whether a context pushed when [`Self::edit_push_state`] was `seen` is out of date.
    #[must_use]
    pub fn edit_push_stale(&self, key: GridKey, seen: &EditPushState) -> bool {
        self.edit_push_state(key) != *seen
    }

    /// Whether the connection of the tab is `prod`, as the gate sees it.
    ///
    /// Derived from the tab's connection and from the saved connection it came from (the stricter
    /// of the two), every time it is asked; never stored, never passed in. A tab that cannot be
    /// found, or an environment label that is not known to be safe, counts as production. A
    /// connection without a label is not production (ADR-0024: only `prod` asks).
    pub(super) fn production(&self, tab: TabId) -> bool {
        let Some(t) = self.tabs.get(tab) else {
            return true;
        };
        let snapshot = t.connection().environment;
        let live = self
            .tab_conns
            .get(&tab)
            .and_then(|conn| self.sidebar.index_of(conn))
            .and_then(|i| self.sidebar.entries().get(i))
            .and_then(|e| e.config().environment);
        snapshot.is_some_and(environment_is_production)
            || live.is_some_and(environment_is_production)
    }

    /// Why an apply on this grid cannot start now, if it cannot.
    fn apply_block(&self, key: GridKey) -> Option<String> {
        if self.edit.commit_unknown.contains(&key) {
            return Some(COMMIT_UNKNOWN.to_owned());
        }
        if self.edit.is_applying(key.tab) {
            return Some(BUSY_APPLYING.to_owned());
        }
        if key.result.is_some() {
            if self
                .queries
                .get(&key.tab)
                .is_none_or(|q| *q.phase() != Phase::Idle)
            {
                return Some(BUSY_RUNNING.to_owned());
            }
            if self.edit.in_tx.contains(&key.tab) {
                return Some(BUSY_TRANSACTION.to_owned());
            }
            if !self.has_session(key.tab) {
                return Some(NO_SESSION.to_owned());
            }
        }
        None
    }

    /// Whether an apply is in flight on the tab.
    #[must_use]
    pub fn is_applying(&self, tab: TabId) -> bool {
        self.edit.is_applying(tab)
    }

    /// Whether the session of the tab may be inside a transaction the user began.
    #[must_use]
    pub fn in_transaction(&self, tab: TabId) -> bool {
        self.edit.in_tx.contains(&tab)
    }

    /// What the grid `key` needs to decide whether it can be edited.
    ///
    /// The effect loads the source table through the catalog (once per table and browser
    /// generation); push a new context after [`Self::edit_generation`] changes. `None` when the
    /// grid has no source (the tab is gone, or it is neither a table-data tab nor a result).
    pub fn edit_context(&mut self, key: GridKey) -> Option<(EditContext, Effect)> {
        let driver = self.tabs.get(key.tab)?.connection().driver.clone();
        let dialect = self.dialect_of(&driver);
        let production = self.production(key.tab);
        let apply_block = self.apply_block(key);
        let catalog = self.editor_catalog(key.tab);
        let mut resolved_schema = None;
        let (origin, object, relation_kind) = match key.result {
            None => {
                let object = self.edit.data_tabs.get(&key.tab)?.clone();
                (EditOrigin::TableData(object.clone()), Some(object), None)
            }
            Some(position) => {
                let sql = self
                    .queries
                    .get(&key.tab)?
                    .results()
                    .get(position)?
                    .sql
                    .clone();
                resolved_schema = self
                    .edit
                    .resolved
                    .get(&(key.tab, position))
                    .cloned()
                    .flatten();
                let found = dialect.zip(catalog.as_ref()).and_then(|(d, (_, c))| {
                    let source = single_table_source(d, &sql)?;
                    let candidates = c
                        .snapshot()
                        .find_relations(&ObjectPath::root(), &source.name.text);
                    pick_relation(
                        d,
                        &source.name,
                        source.schema.as_ref(),
                        resolved_schema.as_deref(),
                        candidates,
                    )
                });
                let kind = found
                    .as_ref()
                    .map(|o| o.kind)
                    .filter(|k| *k != ObjectKind::Table);
                let object = found.filter(|o| o.kind == ObjectKind::Table);
                (EditOrigin::QueryResult(sql), object, kind)
            }
        };
        let mut effect = Effect::None;
        let mut table = None;
        if let (Some(object), Some((generation, catalog))) = (object, catalog) {
            table = catalog.cached_table(&object);
            if table.is_none()
                && let Some(conn) = self.tab_conns.get(&key.tab).cloned()
                && self
                    .edit
                    .table_requests
                    .insert((conn.clone(), generation, object.clone()))
            {
                effect = Effect::LoadCatalog {
                    conn,
                    generation,
                    keys: vec![CacheKey::Table(object)],
                };
            }
        }
        let context = EditContext {
            dialect,
            origin,
            table,
            relation_kind,
            resolved_schema,
            production,
            apply_block,
        };
        Some((context, effect))
    }

    /// The view reports how many changes the grid has pending (after every render or edit).
    ///
    /// Zero also forgets an unknown `COMMIT` outcome: discarding is what unblocks Apply.
    pub fn set_pending_edits(&mut self, key: GridKey, n: usize) {
        if n == 0 {
            self.edit.pending.remove(&key);
            // A dialog for changes that no longer exist must not stay on screen.
            if self.edit.confirm.as_ref().is_some_and(|c| c.key == key) {
                self.edit.confirm = None;
            }
            if self.edit.commit_unknown.remove(&key) {
                self.edit.bump();
            }
        } else {
            self.edit.pending.insert(key, n);
        }
    }

    // ---- preview ----

    /// Opens the preview of a plan (what Apply would run). A plan that could not be built is a
    /// notice. The preview applies nothing.
    pub fn open_preview(&mut self, key: GridKey, plan: Result<ApplyPlan, EditError>) {
        match plan {
            Ok(plan) => {
                self.edit.preview = Some(EditPreview {
                    key,
                    statements: plan.preview(),
                    text: preview_text(&plan),
                });
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    /// The preview on screen.
    #[must_use]
    pub fn edit_preview(&self) -> Option<&EditPreview> {
        self.edit.preview.as_ref()
    }

    /// Closes the preview.
    pub fn close_preview(&mut self) {
        self.edit.preview = None;
    }

    // ---- the gate ----

    fn refuse(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    /// Checks everything about the grid and its tab that an apply needs, now.
    fn check_apply(&self, key: GridKey) -> Result<(), String> {
        if self.tabs.get(key.tab).is_none() {
            return Err("This tab is no longer open.".to_owned());
        }
        match key.result {
            None => {
                if !self.edit.data_tabs.contains_key(&key.tab) {
                    return Err(NO_SOURCE.to_owned());
                }
            }
            Some(position) => {
                let known = self
                    .queries
                    .get(&key.tab)
                    .is_some_and(|q| position < q.results().len());
                if !known {
                    return Err(NO_SOURCE.to_owned());
                }
            }
        }
        self.apply_block(key).map_or(Ok(()), Err)
    }

    /// The single gate of every apply: button, chord, palette and preview all end here.
    ///
    /// `plan` is a plan built for this attempt (`GridEditor::plan`). Returns
    /// [`Effect::ApplyEdits`] when the job can start, [`Effect::None`] otherwise: with a notice
    /// when refused, with [`Self::edit_confirm`] set when the connection is `prod`.
    pub fn request_apply(&mut self, key: GridKey, plan: Result<ApplyPlan, EditError>) -> Effect {
        self.edit.confirm = None;
        if let Err(message) = self.check_apply(key) {
            return self.refuse(message);
        }
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => return self.refuse(error.to_string()),
        };
        // Derived now, from the connection, for this attempt.
        let production = self.production(key.tab);
        if plan.needs_confirmation(production) {
            let connection = self
                .tabs
                .get(key.tab)
                .map(|t| t.connection().name.clone())
                .unwrap_or_default();
            self.edit.preview = None;
            self.edit.confirm = Some(EditConfirm {
                key,
                connection,
                statements: plan.preview(),
                text: preview_text(&plan),
                plan,
            });
            return Effect::None;
        }
        match plan.confirm(production, false) {
            Ok(confirmed) => self.start_apply(key, confirmed),
            // Cannot happen: `needs_confirmation(production)` was false.
            Err(error) => self.refuse(error.to_string()),
        }
    }

    /// The production confirmation on screen.
    #[must_use]
    pub fn edit_confirm(&self) -> Option<&EditConfirm> {
        self.edit.confirm.as_ref()
    }

    /// The user confirmed the production dialog: starts the apply.
    ///
    /// `current` is the plan of the grid now (`GridEditor::plan`). The grid applies what it holds
    /// when the job reports, so the confirmed plan must still be what the grid holds: when the
    /// user edited in between, nothing is written and the user is told to apply again.
    ///
    /// The confirmation is consumed, so a second call finds nothing and returns
    /// [`Effect::None`]. Everything is checked again and `production` is derived again.
    pub fn confirm_apply(&mut self, current: Result<ApplyPlan, EditError>) -> Effect {
        let Some(confirm) = self.edit.confirm.take() else {
            return Effect::None;
        };
        let EditConfirm { key, plan, .. } = confirm;
        if let Err(message) = self.check_apply(key) {
            return self.refuse(message);
        }
        if current.ok().as_ref() != Some(&plan) {
            return self.refuse(CHANGED_SINCE_PREVIEW);
        }
        let production = self.production(key.tab);
        match plan.confirm(production, true) {
            Ok(confirmed) => self.start_apply(key, confirmed),
            Err(error) => self.refuse(error.to_string()),
        }
    }

    /// The user declined the production dialog: nothing is written.
    pub fn dismiss_apply(&mut self) {
        self.edit.confirm = None;
    }

    /// Lends or opens the session and builds the job. Only a confirmed plan gets here.
    fn start_apply(&mut self, key: GridKey, plan: crate::grid::edit::ConfirmedPlan) -> Effect {
        if let Err(message) = self.check_apply(key) {
            return self.refuse(message);
        }
        let session = if key.result.is_some() {
            match self.take_session(key.tab) {
                Some(session) => ApplySession::Lent(session),
                None => return self.refuse(NO_SESSION),
            }
        } else {
            let request = self
                .tab_conns
                .get(&key.tab)
                .ok_or_else(|| NO_SESSION.to_owned())
                .and_then(|conn| self.request_for(conn));
            match request {
                Ok(request) => ApplySession::Connect(request),
                Err(message) => return self.refuse(message),
            }
        };
        self.edit.next_apply += 1;
        let run = ApplyId::new(self.edit.next_apply);
        let stop = ApplyStop::new();
        self.edit.applies.insert(
            key.tab,
            ActiveApply {
                run,
                stop: stop.clone(),
            },
        );
        self.edit.preview = None;
        self.edit.bump();
        Effect::ApplyEdits(ApplyJob {
            key,
            run,
            session,
            plan,
            stop,
        })
    }

    // ---- the answer ----

    /// The apply job reported. The lent session goes back to its tab (or is closed when the tab
    /// is gone); hand `outcome` to the grid's `finish_apply`.
    pub fn apply_finished(&mut self, done: ApplyJobDone) -> ApplyFinished {
        let ApplyJobDone {
            key,
            run,
            outcome,
            session,
        } = done;
        if self
            .edit
            .applies
            .get(&key.tab)
            .is_some_and(|a| a.run == run)
        {
            self.edit.applies.remove(&key.tab);
        }
        let lent = session.is_some();
        let mut effect = Effect::None;
        if let Some(session) = session {
            if self.tabs.get(key.tab).is_some() && self.queries.contains_key(&key.tab) {
                self.restore_session(key.tab, session);
            } else {
                effect = Effect::CloseSession(session);
            }
        }
        // The flag errs toward "in a transaction": when the apply could not start, or a
        // rollback failed, the session may hold a transaction.
        let doubtful = match &outcome {
            ApplyOutcome::NotStarted { .. } => true,
            ApplyOutcome::RolledBack { rollback_error, .. }
            | ApplyOutcome::CommitUnknown { rollback_error, .. } => rollback_error.is_some(),
            _ => false,
        };
        if lent && doubtful {
            self.edit.in_tx.insert(key.tab);
        }
        if matches!(outcome, ApplyOutcome::CommitUnknown { .. }) {
            self.edit.commit_unknown.insert(key);
        }
        if matches!(outcome, ApplyOutcome::Committed { .. }) {
            self.edit.pending.remove(&key);
        }
        self.edit.bump();
        ApplyFinished {
            effect,
            key,
            outcome,
        }
    }

    /// The apply job ended without a report (its task panicked or was dropped). The session is
    /// gone with it, and the outcome of the transaction is unknown: Apply stays off for the grid
    /// until it is discarded. Give [`lost_apply_outcome`] to the grid's `finish_apply`.
    pub fn apply_lost(&mut self, key: GridKey, run: ApplyId) {
        if self
            .edit
            .applies
            .get(&key.tab)
            .is_some_and(|a| a.run == run)
        {
            self.edit.applies.remove(&key.tab);
        }
        self.edit.session_gone(key.tab);
        self.edit.commit_unknown.insert(key);
        self.edit.bump();
        self.notice = Some(format!(
            "The apply ended unexpectedly. {COMMIT_UNKNOWN} Open the connection again."
        ));
    }

    /// Cancels the apply running on a tab.
    pub fn cancel_apply(&mut self, tab: TabId) -> Effect {
        let Some(active) = self.edit.applies.get(&tab) else {
            return self.refuse("No changes are being applied.");
        };
        // Sets the flag the job checks between statements; the handle (once the job has stored
        // the session's) interrupts the statement that is executing.
        match active.stop.cancel() {
            Some(handle) => Effect::CancelQuery(handle),
            None => Effect::None,
        }
    }

    // ---- closing ----

    /// The prompt "discard the pending changes?" on screen.
    #[must_use]
    pub fn close_prompt(&self) -> Option<&ClosePrompt> {
        self.edit.close_prompt.as_ref()
    }

    /// The user agreed to lose the pending changes: closes the tab or quits.
    pub fn confirm_close(&mut self) -> Effect {
        let Some(prompt) = self.edit.close_prompt.take() else {
            return Effect::None;
        };
        match prompt.target {
            CloseTarget::Tab(tab) => {
                if self.edit.is_applying(tab) {
                    return self.refuse(BUSY_APPLYING);
                }
                self.close_tab_now(tab)
            }
            CloseTarget::Quit => self.quit_now(),
        }
    }

    /// The user kept the tab or the application open.
    pub fn dismiss_close(&mut self) {
        self.edit.close_prompt = None;
    }

    /// Closing a tab: asks first when it has pending changes, refuses while it is applying.
    pub(super) fn request_close_tab(&mut self, tab: TabId) -> Effect {
        if self.tabs.get(tab).is_none() {
            return Effect::None;
        }
        if self.edit.is_applying(tab) {
            return self.refuse(BUSY_APPLYING);
        }
        if self.edit.tab_prompt(tab).is_some() {
            return Effect::None;
        }
        self.close_tab_now(tab)
    }

    /// Quitting: asks first when any grid has pending changes.
    pub(super) fn request_quit(&mut self) -> Effect {
        if self.edit.quit_prompt().is_some() {
            return Effect::None;
        }
        self.quit_now()
    }

    // ---- guards used by the run flow ----

    /// Why a query cannot (re)run on the tab now because of editing, if it cannot.
    pub(super) fn run_blocked_by_edits(&self, tab: TabId) -> Option<String> {
        if self.edit.is_applying(tab) {
            return Some(BUSY_APPLYING.to_owned());
        }
        self.edit.pending_notice(tab)
    }

    /// Follows the statements of a run to know whether the session is inside a transaction.
    pub(super) fn follow_transaction(
        &mut self,
        tab: TabId,
        event: &RunEvent,
        statement: Option<(String, bool)>,
    ) {
        let Some((sql, started)) = statement else {
            return;
        };
        let Some(dialect) = self
            .tabs
            .get(tab)
            .and_then(|t| self.dialect_of(&t.connection().driver))
        else {
            return;
        };
        let finished = matches!(event, RunEvent::Finished { .. });
        let stopped =
            finished || matches!(event, RunEvent::Failed { .. } | RunEvent::Cancelled { .. });
        let ended = finished || matches!(event, RunEvent::Failed { .. });
        let changed = self.edit.follow(
            tab,
            tx_effect(dialect, &sql),
            started && stopped,
            ended,
            finished,
        );
        if changed {
            self.edit.bump();
        }
    }

    /// What the tab knew about statement `event_index` before the event: its text, and whether
    /// it had started.
    pub(super) fn statement_before(&self, tab: TabId, event: &RunEvent) -> Option<(String, bool)> {
        let index = event_index(event);
        let entry = self
            .queries
            .get(&tab)?
            .results()
            .iter()
            .find(|r| r.index == index)?;
        Some((
            entry.sql.clone(),
            matches!(entry.status, StatementStatus::Running),
        ))
    }

    /// Keeps the schema the server resolved a result's table name to.
    pub(super) fn keep_resolved(
        &mut self,
        tab: TabId,
        run: RunId,
        index: usize,
        schema: Option<String>,
    ) {
        if !self.is_current_run(tab, run) {
            return;
        }
        let Some(position) = self
            .queries
            .get(&tab)
            .and_then(|q| q.results().iter().position(|r| r.index == index))
        else {
            return;
        };
        self.edit.resolved.insert((tab, position), schema);
        self.edit.bump();
    }
}
