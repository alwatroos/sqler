// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The inline-editing half of the root view (ADR-0033): which grid is edited, the edit context
//! pushed into it, the dialogs' state, the apply job and its answer.
//!
//! Every apply starts at `Workspace::request_apply` with the plan the grid builds for that
//! attempt. This file never builds a statement and never prints a plan, a statement or a value
//! with `{:?}`.

use std::collections::HashMap;

use gpui::{Context, EntityId, FocusHandle, Focusable as _, ListState, Window};
use gpui_component::WindowExt as _;

use super::WorkspaceView;
use crate::grid::GridAction;
use crate::grid::edit::view::{cancel_cell_edit, commit_cell_edit, is_editing, new_statement_list};
use crate::grid::edit::{ApplyId, ApplyJobDone, EditorKey, GridKey, cell_editor_key};
use crate::grid::view::{Table, apply_action};
use crate::keymap::input::key_input;
use crate::workspace::{EditPushState, Effect, lost_apply_outcome};

/// The view state of the editing dialogs and the contexts already pushed to the grids.
pub(super) struct EditUi {
    /// What each grid was last given a context for, and the table it went to.
    pushed: HashMap<GridKey, (EditPushState, EntityId)>,
    /// The grid whose "edit without a key" warning is on screen.
    keyless: Option<(GridKey, Table)>,
    /// The focus target of the dialogs (their Cancel button).
    cancel_focus: FocusHandle,
    preview_list: ListState,
    confirm_list: ListState,
    /// What each list was last sized for: the number of statements and the length of the text.
    preview_shown: Option<(usize, usize)>,
    confirm_shown: Option<(usize, usize)>,
    /// Whether a dialog was open at the last render.
    was_open: bool,
    /// The cell-editor command an action just ran; the key-down of that same key is ignored.
    key_done: Option<EditorKey>,
}

impl EditUi {
    pub(super) fn new(cx: &mut gpui::App) -> Self {
        Self {
            pushed: HashMap::new(),
            keyless: None,
            cancel_focus: cx.focus_handle(),
            preview_list: new_statement_list(),
            confirm_list: new_statement_list(),
            preview_shown: None,
            confirm_shown: None,
            was_open: false,
            key_done: None,
        }
    }
}

impl WorkspaceView {
    // ---- which grid ----

    /// The grid the edit commands act on: the active grid tab, else the shown result of the
    /// active query tab.
    pub(super) fn active_edit_grid(&self) -> Option<(GridKey, Table)> {
        if let Some(tab) = self.ws_active_grid() {
            let table = self.grids.table(tab)?.clone();
            return Some((GridKey { tab, result: None }, table));
        }
        let tab = self.ws_active_query()?;
        let index = self.queries.shown_index(&self.ws, tab)?;
        let table = self.queries.table(tab, index)?.clone();
        Some((
            GridKey {
                tab,
                result: Some(index),
            },
            table,
        ))
    }

    /// The table of a grid, if it still exists.
    fn table_for(&self, key: GridKey) -> Option<Table> {
        match key.result {
            None => self.grids.table(key.tab).cloned(),
            Some(index) => self.queries.table(key.tab, index).cloned(),
        }
    }

    // ---- dialogs ----

    /// Whether an editing dialog is on screen.
    pub(super) fn edit_overlay_open(&self) -> bool {
        self.ws.edit_preview().is_some()
            || self.ws.edit_confirm().is_some()
            || self.ws.close_prompt().is_some()
            || self.edit_ui.keyless.is_some()
    }

    /// Whether the keyless warning is on screen.
    pub(crate) fn edit_keyless_pending(&self) -> bool {
        self.edit_ui.keyless.is_some()
    }

    /// The focus target and the two statement lists of the dialogs.
    pub(crate) fn edit_dialog_parts(&self) -> (&FocusHandle, &ListState, &ListState) {
        (
            &self.edit_ui.cancel_focus,
            &self.edit_ui.preview_list,
            &self.edit_ui.confirm_list,
        )
    }

    /// Escape on a dialog: dismisses it, and nothing is written. `true` if one was open.
    pub(super) fn edit_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.ws.edit_confirm().is_some() {
            self.ws.dismiss_apply();
        } else if self.ws.edit_preview().is_some() {
            self.ws.close_preview();
        } else if self.edit_ui.keyless.is_some() {
            self.edit_ui.keyless = None;
        } else if self.ws.close_prompt().is_some() {
            self.ws.dismiss_close();
        } else {
            return false;
        }
        cx.notify();
        true
    }

    pub(crate) fn edit_confirm_dismiss(&mut self, cx: &mut Context<Self>) {
        self.ws.dismiss_apply();
        cx.notify();
    }

    /// The user confirmed the production dialog: the grid's plan now is handed back to the
    /// workspace, which checks it against the confirmed one.
    pub(crate) fn edit_confirm_apply(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.ws.edit_confirm().map(|c| c.key) else {
            return;
        };
        let plan = self
            .table_for(key)
            .map(|t| t.read(cx).delegate().tab.edit().plan());
        let effect = if let Some(plan) = plan {
            self.ws.confirm_apply(plan)
        } else {
            self.ws.dismiss_apply();
            Effect::None
        };
        self.apply(effect, cx);
        self.settle(cx);
    }

    pub(crate) fn edit_preview_close(&mut self, cx: &mut Context<Self>) {
        self.ws.close_preview();
        cx.notify();
    }

    pub(crate) fn edit_preview_copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.ws.edit_preview().map(|p| p.text.clone()) {
            self.apply(Effect::Copy(text), cx);
        }
    }

    /// Apply from the preview: the same gate as the button, with a new plan.
    pub(crate) fn edit_preview_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.ws.edit_preview().map(|p| p.key) else {
            return;
        };
        self.ws.close_preview();
        if let Some(table) = self.table_for(key) {
            self.edit_workspace_action_on(key, &table, GridAction::ApplyChanges, window, cx);
        }
        cx.notify();
    }

    pub(crate) fn edit_keyless_dismiss(&mut self, cx: &mut Context<Self>) {
        self.edit_ui.keyless = None;
        cx.notify();
    }

    /// The user accepted the keyless warning: the grid then matches rows on all their values.
    pub(crate) fn edit_keyless_accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, table)) = self.edit_ui.keyless.take() else {
            return;
        };
        let allowed = table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let (model, editor) = delegate.tab.edit_parts();
            let allowed = editor.allow_keyless(model.columns());
            cx.notify();
            allowed
        });
        if !allowed {
            window.push_notification("This result cannot be edited without a key.", cx);
        }
        cx.notify();
    }

    pub(crate) fn edit_close_dismiss(&mut self, cx: &mut Context<Self>) {
        self.ws.dismiss_close();
        cx.notify();
    }

    pub(crate) fn edit_close_confirm(&mut self, cx: &mut Context<Self>) {
        let effect = self.ws.confirm_close();
        self.apply(effect, cx);
        self.settle(cx);
    }

    pub(crate) fn edit_cancel_apply(&mut self, tab: crate::tabs::TabId, cx: &mut Context<Self>) {
        let effect = self.ws.cancel_apply(tab);
        self.apply(effect, cx);
        self.settle(cx);
    }

    // ---- actions ----

    /// Runs a grid action on a given table (the edit bar's buttons).
    pub(crate) fn edit_action_for(
        &mut self,
        key: GridKey,
        table: &Table,
        action: GridAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.edit_workspace_action_on(key, table, action, window, cx) {
            table.update(cx, |state, cx| apply_action(state, action, window, cx));
            window.focus(&table.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Runs an edit action that needs the workspace on the active grid. `false` if the action is
    /// not one of those (or there is no grid).
    pub(super) fn edit_workspace_action(
        &mut self,
        action: GridAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(
            action,
            GridAction::PreviewChanges | GridAction::ApplyChanges | GridAction::EditWithoutKey
        ) {
            return false;
        }
        let Some((key, table)) = self.active_edit_grid() else {
            return false;
        };
        self.edit_workspace_action_on(key, &table, action, window, cx)
    }

    /// The workspace-level edit actions: preview, apply, and the keyless opt-in.
    fn edit_workspace_action_on(
        &mut self,
        key: GridKey,
        table: &Table,
        action: GridAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match action {
            GridAction::PreviewChanges => {
                let plan = table.read(cx).delegate().tab.edit().plan();
                self.ws.open_preview(key, plan);
            }
            GridAction::ApplyChanges => {
                // A plan built for this attempt; the workspace decides everything else.
                let plan = table.read(cx).delegate().tab.edit().plan();
                let effect = self.ws.request_apply(key, plan);
                self.apply(effect, cx);
            }
            GridAction::EditWithoutKey => {
                self.edit_ui.keyless = Some((key, table.clone()));
            }
            _ => return false,
        }
        self.settle(cx);
        true
    }

    // ---- keys ----

    /// The cell editor's keys (Enter, Tab, Shift+Tab, Escape), when a cell of the active grid
    /// is being edited and the table has the focus. `true` if the key was used.
    pub(super) fn cell_editor_key_event(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let done = self.edit_ui.key_done.take();
        let Some(command) = cell_editor_key(&key_input(&event.keystroke)) else {
            return false;
        };
        if done == Some(command) {
            // The input's own action already ran for this key press (see `cell_editor_action`).
            return true;
        }
        self.run_cell_editor_key(command, window, cx)
    }

    /// The input of the open cell editor ran Enter, Tab, Shift+Tab or Escape as an action. The
    /// grid's commands win; with no cell editor open the action goes on to the input.
    pub(super) fn cell_editor_action(
        &mut self,
        command: EditorKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.run_cell_editor_key(command, window, cx) {
            self.edit_ui.key_done = Some(command);
            cx.stop_propagation();
        } else {
            cx.propagate();
        }
    }

    fn run_cell_editor_key(
        &mut self,
        command: EditorKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.palette.is_open() || self.edit_overlay_open() {
            return false;
        }
        let Some((_, table)) = self.active_edit_grid() else {
            return false;
        };
        if !is_editing(table.read(cx)) || !table.focus_handle(cx).contains_focused(window, cx) {
            return false;
        }
        table.update(cx, |state, cx| match command {
            EditorKey::Commit { then } => {
                commit_cell_edit(state, then, window, cx);
            }
            EditorKey::Cancel => cancel_cell_edit(state, window, cx),
        });
        cx.notify();
        true
    }

    /// Whether a cell of the active grid is being edited.
    pub(super) fn cell_editing(&self, cx: &gpui::App) -> bool {
        self.active_edit_grid()
            .is_some_and(|(_, table)| is_editing(table.read(cx)))
    }

    // ---- render sync ----

    /// Keeps the active grid and the workspace in step: pushes the edit context when it or the
    /// grid changed, reports the pending changes, sizes the statement lists and moves the focus
    /// into and out of the dialogs.
    pub(super) fn sync_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((key, table)) = self.active_edit_grid() {
            let id = table.entity_id();
            let stale = match self.edit_ui.pushed.get(&key) {
                Some((seen, pushed_id)) => *pushed_id != id || self.ws.edit_push_stale(key, seen),
                None => true,
            };
            if stale {
                let before = self.ws.edit_push_state(key);
                if let Some((ctx, effect)) = self.ws.edit_context(key) {
                    table.update(cx, |state, cx| {
                        state.delegate_mut().tab.set_edit_context(ctx);
                        cx.notify();
                    });
                    self.apply(effect, cx);
                }
                // The state from before `edit_context`: a bump it caused re-pushes next frame.
                self.edit_ui.pushed.insert(key, (before, id));
            }
            let pending = table.read(cx).delegate().tab.edit().pending();
            self.ws.set_pending_edits(key, pending);
        }
        if let Some(notice) = self.ws.take_notice() {
            self.notices.push(notice);
        }
        let ui = &mut self.edit_ui;
        if let Some(p) = self.ws.edit_preview() {
            let shown = (p.statements.len(), p.text.len());
            if ui.preview_shown != Some(shown) {
                ui.preview_list.reset(shown.0);
                ui.preview_shown = Some(shown);
            }
        } else {
            ui.preview_shown = None;
        }
        if let Some(c) = self.ws.edit_confirm() {
            let shown = (c.statements.len(), c.text.len());
            if ui.confirm_shown != Some(shown) {
                ui.confirm_list.reset(shown.0);
                ui.confirm_shown = Some(shown);
            }
        } else {
            ui.confirm_shown = None;
        }
        let open = self.edit_overlay_open();
        if open && !self.edit_ui.was_open {
            window.focus(&self.edit_ui.cancel_focus, cx);
        } else if !open && self.edit_ui.was_open {
            match self.active_edit_grid() {
                Some((_, table)) => window.focus(&table.focus_handle(cx), cx),
                None => window.focus(&self.focus, cx),
            }
        }
        self.edit_ui.was_open = open;
    }

    // ---- the apply job ----

    /// Starts an apply job on tokio. The grid learns of it first (`begin_apply`), then of the
    /// outcome (`finish_apply`), which is also shown as a notice.
    pub(super) fn start_apply_job(
        &mut self,
        job: crate::grid::edit::ApplyJob,
        cx: &mut Context<Self>,
    ) {
        let (key, run) = (job.key, job.run);
        let rows = job.statement_rows();
        if let Some(table) = self.table_for(key) {
            table.update(cx, |state, cx| {
                state.delegate_mut().cell_editor = None;
                state.delegate_mut().tab.edit_parts().1.begin_apply(rows);
                cx.notify();
            });
        }
        let done = self.bridge.spawn(job.run(self.bridge.connector()));
        cx.spawn(async move |this, cx| {
            let result = done.await.ok();
            this.update(cx, |view, cx| view.apply_done(key, run, result, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// The job reported (`Some`) or ended without a report (`None`).
    fn apply_done(
        &mut self,
        key: GridKey,
        run: ApplyId,
        done: Option<ApplyJobDone>,
        cx: &mut Context<Self>,
    ) {
        let outcome = if let Some(done) = done {
            let finished = self.ws.apply_finished(done);
            self.apply(finished.effect, cx);
            finished.outcome
        } else {
            self.ws.apply_lost(key, run);
            lost_apply_outcome()
        };
        if let Some(table) = self.table_for(key) {
            let message = table.update(cx, |state, cx| {
                let message = state
                    .delegate_mut()
                    .tab
                    .edit_parts()
                    .1
                    .finish_apply(&outcome);
                cx.notify();
                message
            });
            self.notices.push(message);
        }
        self.settle(cx);
    }
}
