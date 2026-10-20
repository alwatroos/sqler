// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The command palette overlay: a top-centre panel with the query input and the matching rows.

use gpui::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Input, InputState};

use super::{PaletteItem, PaletteState};
use crate::command::CommandId;
use crate::workspace::WorkspaceView;

/// Height of one row, in pixels.
const ROW_HEIGHT: f32 = 28.;
/// Rows visible before the list scrolls.
const VISIBLE_ROWS: f32 = 12.;

/// What a click on a row does: run the command through the view.
pub(crate) type PickFn =
    fn(&mut WorkspaceView, &CommandId, &mut Window, &mut Context<WorkspaceView>);

/// Renders the palette overlay. Rows are the already computed `items`; a click calls `pick`.
pub(crate) fn palette_overlay(
    state: &PaletteState,
    items: &[PaletteItem],
    input: &Entity<InputState>,
    pick: PickFn,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (bg, fg, border, muted) = (
        theme.popover,
        theme.popover_foreground,
        theme.border,
        theme.muted_foreground,
    );
    let (selected_bg, hover_bg) = (theme.list_active, theme.list_hover);
    let selected = state.selected();

    let mut list = div()
        .id("palette-list")
        .flex()
        .flex_col()
        .max_h(px(ROW_HEIGHT * VISIBLE_ROWS))
        .overflow_y_scroll();
    for (index, item) in items.iter().enumerate() {
        let id = item.id.clone();
        let mut row = div()
            .id(("palette-row", index))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .px_3()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(move |view, _: &gpui::ClickEvent, window, cx| {
                pick(view, &id, window, cx);
            }))
            .child(div().text_sm().child(item.title.clone()))
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(muted)
                    .child(item.id.to_string()),
            );
        if index == selected {
            row = row.bg(selected_bg);
        }
        if let Some(shortcut) = &item.shortcut {
            row = row.child(div().text_xs().text_color(muted).child(shortcut.clone()));
        }
        list = list.child(row);
    }

    div()
        .absolute()
        .top(px(48.))
        .left_0()
        .w_full()
        .flex()
        .flex_row()
        .justify_center()
        .child(
            div()
                .id("palette")
                .w(px(600.))
                .flex()
                .flex_col()
                .bg(bg)
                .text_color(fg)
                .border_1()
                .border_color(border)
                .rounded_md()
                .shadow_lg()
                .child(div().p_2().child(Input::new(input)))
                .child(list),
        )
}
