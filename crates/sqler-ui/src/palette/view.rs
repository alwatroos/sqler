// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The command palette overlay: a top-centre panel with the query input and the matching rows.

use gpui::{
    Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Input, InputState};

use super::sections::{PaletteLine, palette_lines};
use super::{PaletteItem, PaletteState};
use crate::command::CommandId;
use crate::theme::SPACE;
use crate::theme::widgets::kbd;
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{OVERLAY_ROW, OVERLAY_TOP, OVERLAY_W, ROW_HEIGHT, ui_scale};

/// Rows visible before the list scrolls.
const VISIBLE_ROWS: f32 = 12.;
/// The line shown when no command matches.
const NO_MATCH: &str = "No matching commands";

/// What a click on a row does: run the command through the view.
pub(crate) type PickFn =
    fn(&mut WorkspaceView, &CommandId, &mut Window, &mut Context<WorkspaceView>);

#[allow(clippy::too_many_lines)] // one declarative element tree
/// Renders the palette overlay. Rows are the already computed `items`; a click calls `pick`.
pub(crate) fn palette_overlay(
    state: &PaletteState,
    items: &[PaletteItem],
    input: &Entity<InputState>,
    pick: PickFn,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (bg, fg, border, muted) = (
        theme.popover,
        theme.popover_foreground,
        theme.border,
        theme.muted_foreground,
    );
    let (selected_bg, hover_bg) = (theme.list_active, theme.list_hover);
    let radius = theme.radius_lg;
    let selected = state.selected();

    let mut list = div()
        .id("palette-list")
        .flex()
        .flex_col()
        .max_h(px(OVERLAY_ROW * VISIBLE_ROWS))
        .overflow_y_scroll();
    if items.is_empty() {
        list = list.child(
            div()
                .h(px(OVERLAY_ROW))
                .px(px(SPACE[4]))
                .flex()
                .items_center()
                .text_size(px(scale.sm))
                .text_color(muted)
                .child(NO_MATCH),
        );
    }
    for line in palette_lines(items) {
        let index = match line {
            PaletteLine::Section(label) => {
                list = list.child(
                    div()
                        .h(px(ROW_HEIGHT))
                        .flex_none()
                        .px(px(SPACE[4]))
                        .flex()
                        .items_center()
                        .text_size(px(scale.xs))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child(label.to_uppercase()),
                );
                continue;
            }
            PaletteLine::Item(index) => index,
        };
        let item = &items[index];
        let id = item.id.clone();
        let mut row = div()
            .id(("palette-row", index))
            .h(px(OVERLAY_ROW))
            .flex_none()
            .px(px(SPACE[4]))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE[3]))
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(move |view, _: &gpui::ClickEvent, window, cx| {
                pick(view, &id, window, cx);
            }))
            .child(
                div()
                    .text_size(px(scale.sm))
                    .text_color(fg)
                    .child(item.title.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(scale.xs))
                    .text_color(muted)
                    .child(item.id.to_string()),
            );
        if index == selected {
            row = row.bg(selected_bg);
        }
        if let Some(shortcut) = &item.shortcut {
            row = row.child(kbd(shortcut.clone(), cx));
        }
        list = list.child(row);
    }

    div()
        .absolute()
        .top(px(OVERLAY_TOP))
        .left_0()
        .w_full()
        .flex()
        .flex_row()
        .justify_center()
        .child(
            div()
                .id("palette")
                .w(px(OVERLAY_W))
                .flex()
                .flex_col()
                .bg(bg)
                .text_color(fg)
                .border_1()
                .border_color(border)
                .rounded(radius)
                .child(div().p(px(SPACE[3])).child(Input::new(input)))
                .child(list),
        )
}
