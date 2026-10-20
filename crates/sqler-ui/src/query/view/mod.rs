// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The query tab views: the editor on top, the results below, and the two overlays a run can
//! open (the parameter prompt and the production confirmation).
//!
//! These only render state. Clicks call back into `WorkspaceView`, which goes through the
//! `Workspace` methods of `workspace/query.rs`.

mod dialogs;
mod results;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Styled as _, div, px,
};
use gpui_component::ActiveTheme as _;

pub(crate) use dialogs::query_overlay;
pub(crate) use results::{OverlayChange, QueryViews};

use crate::tabs::TabId;
use crate::workspace::WorkspaceView;

/// The body of a query tab: `editor` above the results of the last run.
///
/// The editor is the SQL editor element of the tab (`EditorViews::body`).
pub(crate) fn query_tab_body(
    editor: AnyElement,
    tab: TabId,
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let border = cx.theme().border;
    div()
        .id("query-tab")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .id("query-editor")
                .flex_none()
                .w_full()
                .h(px(220.))
                .border_b_1()
                .border_color(border)
                .child(editor),
        )
        .child(results::results_area(view, tab, cx))
        .into_any_element()
}
