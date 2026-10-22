// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The workbench shell of the root view: resizable splits, editor groups, the bottom panel body,
//! focus regions (`F6`) and the debounced write of `workbench.toml`. Every decision lives in the
//! headless `workbench` and `tabs` modules; this file only arranges their render functions.

use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Context, FocusHandle, InteractiveElement as _,
    IntoElement as _, MouseButton, ParentElement as _, SharedString, Styled as _, Window, div, px,
};
use gpui_component::resizable::{
    ResizablePanelEvent, ResizableState, h_resizable, resizable_panel, v_resizable,
};
use sqler_config::Modifiers;

use super::WorkspaceView;
use super::audit::audit_panel_body;
use crate::command::CommandId;
use crate::export::view::export_bar;
use crate::grid::IngestState;
use crate::history::view::history_panel_body;
use crate::query::StatementStatus;
use crate::query::view::{query_editor_body, query_results_panel};
use crate::settings;
use crate::tabs::{self, GroupId, TabId, TabKind};
use crate::workbench::view::{
    activity_bar, bottom_panel, focus_index, messages_body, side_panel, status_bar, welcome,
};
use crate::workbench::{
    PANEL_MAX_FRACTION, PANEL_MIN, PanelView, Region, RowInfo, SIDE_MAX, SIDE_MIN, WorkbenchAction,
    next_region,
};
use crate::workspace::WorkspaceAction;

/// How long a change of the layout waits before `workbench.toml` is written.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// The regions that have a handle of their own; the editor region is the groups.
const HANDLE_REGIONS: [Region; 4] = [
    Region::ActivityBar,
    Region::SidePanel,
    Region::Panel,
    Region::StatusBar,
];

impl WorkspaceView {
    /// Creates the two split states afresh (so `size()` takes effect) and follows their drags.
    pub(super) fn bind_splits(&mut self, cx: &mut Context<Self>) {
        self.side_split = cx.new(|_| ResizableState::default());
        self.panel_split = cx.new(|_| ResizableState::default());
        self.split_generation = self.ws.layout().generation();
        self.split_subs = vec![
            cx.subscribe(&self.side_split, |view, _, _: &ResizablePanelEvent, cx| {
                view.side_resized(cx);
            }),
            cx.subscribe(&self.panel_split, |view, _, _: &ResizablePanelEvent, cx| {
                view.panel_resized(cx);
            }),
        ];
    }

    /// A drag of the side panel divider ended: the width goes to the layout.
    fn side_resized(&mut self, cx: &mut Context<Self>) {
        if !self.ws.layout().side_visible() {
            return;
        }
        let Some(width) = self.side_split.read(cx).sizes().first().copied() else {
            return;
        };
        self.dispatch_workbench(WorkbenchAction::SetSideWidth(to_u16(f32::from(width))), cx);
        self.split_generation = self.ws.layout().generation();
    }

    /// A drag of the bottom panel divider ended: the height goes to the layout.
    fn panel_resized(&mut self, cx: &mut Context<Self>) {
        if !self.ws.layout().panel_visible() {
            return;
        }
        let Some(height) = self.panel_split.read(cx).sizes().get(1).copied() else {
            return;
        };
        self.dispatch_workbench(
            WorkbenchAction::SetPanelHeight(to_u16(f32::from(height))),
            cx,
        );
        self.split_generation = self.ws.layout().generation();
    }

    /// The whole shell below the overlays: activity bar, side panel, editor groups, bottom panel
    /// and status bar.
    pub(super) fn shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.split_generation != self.ws.layout().generation() {
            self.bind_splits(cx);
        }
        let layout = self.ws.layout().clone();
        let window_height = f32::from(window.viewport_size().height);
        let rows = self.row_info(cx);
        let model = self.ws.status_bar(rows);

        let activity = activity_bar(
            self,
            &self.hints,
            &self.activity_focus,
            &self.activity_items,
            cx,
        );
        let status = status_bar(
            self,
            &model,
            &self.hints,
            &self.status_focus,
            &self.status_first,
            cx,
        );
        let editors = self.editor_area(window, cx);

        let mut vertical = v_resizable("panel-split")
            .with_state(&self.panel_split)
            .child(
                resizable_panel()
                    .size_range(px(PANEL_MIN)..gpui::Pixels::MAX)
                    .child(fill(editors)),
            );
        if layout.panel_visible() {
            let body = self.panel_body(cx);
            let panel = bottom_panel(self, body, &self.panel_focus, cx);
            let max = (window_height * PANEL_MAX_FRACTION).max(PANEL_MIN);
            vertical = vertical.child(
                resizable_panel()
                    .size(px(layout.panel_height_in(window_height)))
                    .size_range(px(PANEL_MIN)..px(max))
                    .fixed()
                    .child(fill(panel)),
            );
        }
        let mut horizontal = h_resizable("side-split").with_state(&self.side_split);
        if layout.side_visible() {
            let side = side_panel(self, cx);
            horizontal = horizontal.child(
                resizable_panel()
                    .size(px(layout.side_width()))
                    .size_range(px(SIDE_MIN)..px(SIDE_MAX))
                    .fixed()
                    .child(fill(side)),
            );
        }
        horizontal = horizontal.child(
            resizable_panel()
                .size_range(px(PANEL_MIN)..gpui::Pixels::MAX)
                .child(fill(vertical.into_any_element())),
        );

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_row()
                    .child(activity)
                    .child(div().flex_1().min_w_0().h_full().child(horizontal)),
            )
            .child(status)
            .into_any_element()
    }

    /// The row counts of the result in view, for the status bar. A query tab counts only when
    /// the statement in view finished with a result set; a failed or running one has none.
    fn row_info(&self, cx: &App) -> Option<RowInfo> {
        let table = self.active_table()?;
        let grid_tab = self.ws_active_grid().is_some();
        let tab = &table.read(cx).delegate().tab;
        if !grid_tab {
            let query = self.ws_active_query()?;
            let index = self.queries.shown_index(&self.ws, query)?;
            let status = &self.ws.query_tab(query)?.results().get(index)?.status;
            if !counts_rows(status, tab.column_count()) {
                return None;
            }
        }
        Some(RowInfo {
            loaded: tab.row_count() as u64,
            shown: tab.display_row_count() as u64,
            more: matches!(tab.state(), IngestState::Paused),
        })
    }

    /// The editor groups, or the welcome view when nothing is open.
    fn editor_area(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.ws.tabs().is_empty() && !self.ws.settings_tab_open() {
            return welcome(self, &self.hints, cx);
        }
        let ids: Vec<GroupId> = self
            .ws
            .tabs()
            .groups()
            .iter()
            .map(tabs::Group::id)
            .collect();
        self.tab_scroll.retain(|g, _| ids.contains(g));
        self.last_active.retain(|g, _| ids.contains(g));
        let groups: Vec<AnyElement> = ids
            .iter()
            .enumerate()
            .map(|(position, g)| self.group_view(*g, position, window, cx))
            .collect();
        if groups.len() == 1 {
            return groups
                .into_iter()
                .next()
                .unwrap_or_else(|| div().into_any_element());
        }
        let mut split = h_resizable("groups");
        for group in groups {
            split = split.child(resizable_panel().child(fill(group)));
        }
        split.into_any_element()
    }

    /// One editor group: its tab bar above the body of its active tab.
    fn group_view(
        &mut self,
        group: GroupId,
        position: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scroll = self.tab_scroll.entry(group).or_default().clone();
        let bar = tabs::view::tab_bar(self, group, &scroll, cx);
        let active = self.ws.tabs().active_in(group).map(tabs::Tab::id);
        if let Some(id) = active
            && self.last_active.get(&group) != Some(&id)
        {
            self.last_active.insert(group, id);
            if let Some(index) = self
                .ws
                .tabs()
                .groups()
                .get(position)
                .and_then(|g| g.tabs().iter().position(|t| *t == id))
            {
                scroll.scroll_to_item(index);
            }
        }
        let progress = active.and_then(|tab| export_bar(self, tab, cx));
        let body = self.group_body(group, position, active, window, cx);
        div()
            .id(("group", position))
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, _, cx| {
                    if view.ws.tabs().focused_group() != group {
                        view.dispatch(WorkspaceAction::FocusGroup(group), cx);
                    }
                }),
            )
            .child(bar)
            .children(progress)
            .child(body)
            .into_any_element()
    }

    /// The body of the active tab of `group`.
    fn group_body(
        &mut self,
        group: GroupId,
        position: usize,
        active: Option<TabId>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if position == 0 && self.ws.settings_tab_active() {
            return settings::view::page(self, cx).into_any_element();
        }
        let Some(tab) = active else {
            return self.fallback_body(group, cx);
        };
        let kind = self.ws.tabs().get(tab).map(tabs::Tab::kind);
        if kind == Some(TabKind::AiSession) {
            return crate::ai::session::view::session_tab_body(self, tab, cx);
        }
        if kind == Some(TabKind::Query)
            && let Some(editor) = self.editors.body(tab)
        {
            return query_editor_body(self, tab, editor.into_any_element(), cx);
        }
        if kind == Some(TabKind::Grid) {
            return self.grids.body(self, tab, cx).into_any_element();
        }
        if let Some(body) = tabs::view::object_tab_body(self, tab, cx) {
            return body;
        }
        self.fallback_body(group, cx)
    }

    /// What a group shows when its tab has no body yet.
    fn fallback_body(&self, group: GroupId, cx: &mut Context<Self>) -> AnyElement {
        if self.ws.tabs().focused_group() == group {
            tabs::view::tab_body(self.ws.tabs(), cx).into_any_element()
        } else {
            div().flex_1().into_any_element()
        }
    }

    /// The body of the bottom panel view the layout shows.
    fn panel_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.ws.tabs().active().map(tabs::Tab::id);
        match self.ws.layout().panel_view() {
            PanelView::Results => {
                let hint = self.run_hint();
                query_results_panel(self, active, hint, cx)
            }
            PanelView::Messages => messages_body(self, active, cx),
            PanelView::History => history_panel_body(self, cx),
            PanelView::Audit => audit_panel_body(self, cx),
        }
    }

    /// The shortcut of the command that runs a query, for the empty results state.
    fn run_hint(&self) -> Option<SharedString> {
        let id = CommandId::new("query:run-statement").ok()?;
        self.keys.shortcut_display(&id).map(SharedString::from)
    }

    /// Carries out a `workbench:*` action: the focus actions move the focus, the others change
    /// the layout.
    pub(super) fn run_workbench(
        &mut self,
        action: WorkbenchAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            WorkbenchAction::FocusNextRegion | WorkbenchAction::FocusPreviousRegion => {
                let forward = action == WorkbenchAction::FocusNextRegion;
                let current = self.current_region(window, cx);
                if let Some(handle) = window.focused(cx) {
                    self.region_memory.insert(current, handle);
                }
                let next = next_region(current, self.ws.layout(), forward);
                self.focus_region(next, window, cx);
            }
            other => {
                self.dispatch_workbench(other, cx);
                // A hidden panel must not keep the focus: the keys would go nowhere.
                let layout = self.ws.layout();
                let side_lost =
                    !layout.side_visible() && self.sidebar_focus.contains_focused(window, cx);
                let panel_lost =
                    !layout.panel_visible() && self.panel_focus.contains_focused(window, cx);
                if side_lost || panel_lost {
                    self.focus_region(Region::Editor, window, cx);
                }
            }
        }
    }

    /// The handle of a region that has one.
    fn region_handle(&self, region: Region) -> Option<&FocusHandle> {
        match region {
            Region::ActivityBar => Some(&self.activity_focus),
            Region::SidePanel => Some(&self.sidebar_focus),
            Region::Panel => Some(&self.panel_focus),
            Region::StatusBar => Some(&self.status_focus),
            Region::Editor => None,
        }
    }

    /// The region that holds the focus; the editor groups when no other region does.
    pub(super) fn current_region(&self, window: &Window, cx: &App) -> Region {
        HANDLE_REGIONS
            .into_iter()
            .find(|r| {
                self.region_handle(*r)
                    .is_some_and(|h| h.contains_focused(window, cx))
            })
            .unwrap_or(Region::Editor)
    }

    /// Moves the focus into `region`: where it was when the region was left, else its default.
    pub(super) fn focus_region(
        &mut self,
        region: Region,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remembered = self.region_handle(region).and_then(|root| {
            let handle = self.region_memory.get(&region)?;
            (handle == root || root.contains(handle, window)).then(|| handle.clone())
        });
        if let Some(handle) = remembered {
            window.focus(&handle, cx);
            return;
        }
        match region {
            Region::ActivityBar => {
                let item = self.activity_items.get(focus_index(self));
                window.focus(item.unwrap_or(&self.activity_focus), cx);
            }
            Region::SidePanel => window.focus(&self.sidebar_focus, cx),
            Region::StatusBar => window.focus(&self.status_first, cx),
            Region::Panel => {
                let table = (self.ws.layout().panel_view() == PanelView::Results)
                    .then(|| {
                        self.ws_active_query()
                            .and_then(|tab| self.queries.shown_table(&self.ws, tab))
                    })
                    .flatten();
                match table {
                    Some(table) => {
                        use gpui::Focusable as _;
                        window.focus(&table.focus_handle(cx), cx);
                    }
                    None => window.focus(&self.panel_focus, cx),
                }
            }
            Region::Editor => self.focus_editor_region(window, cx),
        }
    }

    /// Focuses the active editor, else the active grid, else the root.
    fn focus_editor_region(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui::Focusable as _;
        if let Some(tab) = self.ws_active_editor() {
            self.editors.focus(tab, window, cx);
        } else if let Some(table) = self
            .ws_active_grid()
            .and_then(|tab| self.grids.table(tab).cloned())
        {
            window.focus(&table.focus_handle(cx), cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }

    /// The keys of the regions: Escape leaves the bottom panel, Tab walks the chrome. `true` if
    /// the key was used.
    pub(super) fn region_key(
        &mut self,
        chord: &sqler_config::KeyChord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let plain = chord.modifiers() == Modifiers::default();
        match chord.key() {
            "escape" if plain && self.panel_focus.contains_focused(window, cx) => {
                self.focus_editor_region(window, cx);
                true
            }
            "tab" => {
                let shift = chord.modifiers()
                    == Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    };
                let chrome = self.activity_focus.contains_focused(window, cx)
                    || self.status_focus.contains_focused(window, cx)
                    || self.focus.is_focused(window);
                if !(plain || shift) || !chrome {
                    return false;
                }
                if shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                true
            }
            _ => false,
        }
    }

    /// Writes `workbench.toml` after a pause when the layout, the recent lists or the window
    /// size differ from what is on disk. A file from a newer `SQLer` is never written.
    pub(super) fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.workbench_dir.clone() else {
            return;
        };
        let Some(state) = self
            .ws
            .workbench_state(self.palette.recent(), self.window_size.clone())
        else {
            return;
        };
        if self.saved_state.as_ref() == Some(&state) {
            // A save scheduled for a state the user has since undone must not land.
            self.save_task = None;
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let to_write = state.clone();
            let result = cx
                .background_executor()
                .spawn(async move { dir.save_workbench_state(&to_write) })
                .await;
            this.update(cx, |view, cx| {
                // Remembered even on failure: the next change retries, but the user hears once.
                view.saved_state = Some(state);
                match result {
                    Ok(()) => view.save_failed = false,
                    Err(err) if !view.save_failed => {
                        view.save_failed = true;
                        view.notices
                            .push(format!("Could not save the layout (workbench.toml): {err}"));
                        cx.notify();
                    }
                    Err(_) => {}
                }
            })
            .ok();
        }));
    }

    /// Writes `workbench.toml` now (the app is quitting).
    pub(super) fn save_workbench_now(&mut self) {
        self.save_task = None;
        let Some(dir) = self.workbench_dir.as_ref() else {
            return;
        };
        let Some(state) = self
            .ws
            .workbench_state(self.palette.recent(), self.window_size.clone())
        else {
            return;
        };
        if self.saved_state.as_ref() != Some(&state) && dir.save_workbench_state(&state).is_ok() {
            self.saved_state = Some(state);
        }
    }
}

/// Wraps `element` so it fills its resizable panel and may shrink.
fn fill(element: AnyElement) -> AnyElement {
    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .child(element)
        .into_any_element()
}

/// Whether the row count of a statement is worth showing: it finished and produced columns.
fn counts_rows(status: &StatementStatus, columns: usize) -> bool {
    matches!(status, StatementStatus::Done { .. }) && columns > 0
}

/// A size in pixels as the whole number the layout stores.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to u16
fn to_u16(v: f32) -> u16 {
    v.round().clamp(0., f32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{counts_rows, to_u16};
    use crate::query::StatementStatus;

    #[test]
    fn rows_show_only_for_a_finished_result_set() {
        let done = StatementStatus::Done {
            rows: 0,
            rows_affected: None,
            duration: Duration::ZERO,
        };
        let failed = StatementStatus::Failed {
            message: "boom".into(),
            duration: Duration::ZERO,
        };
        assert!(counts_rows(&done, 2));
        assert!(!counts_rows(&done, 0));
        assert!(!counts_rows(&failed, 2));
        assert!(!counts_rows(&StatementStatus::Running, 2));
        assert!(!counts_rows(&StatementStatus::Cancelled, 2));
    }

    #[test]
    fn sizes_round_and_clamp() {
        assert_eq!(to_u16(259.6), 260);
        assert_eq!(to_u16(-5.), 0);
        assert_eq!(to_u16(f32::MAX), u16::MAX);
        assert_eq!(to_u16(f32::NAN), 0);
    }
}
