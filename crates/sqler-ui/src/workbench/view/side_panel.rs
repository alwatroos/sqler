// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The side panel: the Connections, Saved Queries or AI view.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Styled as _, div,
};
use gpui_component::ActiveTheme as _;

use crate::ai::hub::view::ai_view;
use crate::history::view::saved_section;
use crate::sidebar::view::sidebar;
use crate::theme::widgets::section_header;
use crate::workbench::SideView;
use crate::workspace::WorkspaceView;

/// The view the layout selects.
pub(crate) fn side_panel(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> AnyElement {
    match view.workspace().layout().side_view() {
        SideView::Connections => sidebar(view, cx).into_any_element(),
        SideView::Ai => ai_view(view, cx),
        SideView::SavedQueries => {
            let theme = cx.theme();
            let (bg, fg, border) = (
                theme.sidebar,
                theme.sidebar_foreground,
                theme.sidebar_border,
            );
            div()
                .id("saved-queries-view")
                .track_focus(view.sidebar_focus())
                .size_full()
                .flex()
                .flex_col()
                .bg(bg)
                .text_color(fg)
                .border_r_1()
                .border_color(border)
                .child(section_header("SAVED QUERIES", cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(saved_section(view, cx)),
                )
                .into_any_element()
        }
    }
}
