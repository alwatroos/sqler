// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The query tab views: the editor on top, the results below, and the two overlays a run can
//! open (the parameter prompt and the production confirmation).
//!
//! These only render state. Clicks call back into `WorkspaceView`, which goes through the
//! `Workspace` methods of `workspace/query.rs`.

mod dialogs;
mod results;
mod toolbar;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, div,
};

pub(crate) use dialogs::{query_overlay, scrim};
pub(crate) use results::{OverlayChange, QueryViews};

use crate::tabs::TabId;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{empty_state, kbd};
use crate::workspace::WorkspaceView;

/// The body of a query tab: the toolbar, then the SQL editor at full height. The results live in
/// the bottom panel.
pub(crate) fn query_editor_body(
    view: &WorkspaceView,
    tab: TabId,
    editor: AnyElement,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    div()
        .id("query-editor-body")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(toolbar::query_toolbar(view, tab, cx))
        .children(crate::ai::editor::view::prompt_bar(view, tab, cx))
        .child(
            div()
                .id("query-editor")
                .flex_1()
                .min_h_0()
                .w_full()
                .child(editor),
        )
        .into_any_element()
}

/// The Results view of the bottom panel for the active tab.
///
/// `run_hint` is the shortcut that runs a query, shown in the empty state.
pub(crate) fn query_results_panel(
    view: &WorkspaceView,
    tab: Option<TabId>,
    run_hint: Option<SharedString>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let query = tab.filter(|id| view.workspace().query_tab(*id).is_some());
    if let Some(id) = query
        && view
            .workspace()
            .query_tab(id)
            .is_some_and(|q| !q.results().is_empty())
    {
        return results::results_area(view, id, cx);
    }
    let (title, line) = if tab.is_some() && query.is_none() {
        ("No results for this tab", "Results show for query tabs.")
    } else {
        (
            "Run a query to see results here",
            "Run the statement at the cursor, the selection or the whole script.",
        )
    };
    let mut state = empty_state(AppIcon::Play, title, line, cx);
    if (query.is_some() || tab.is_none())
        && let Some(hint) = run_hint
    {
        state = state.child(kbd(hint, cx));
    }
    div()
        .id("query-results")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(state)
        .into_any_element()
}
