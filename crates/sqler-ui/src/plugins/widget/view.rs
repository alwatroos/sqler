// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Draws the widget tree of a plugin panel (ADR-0060) with the application's own components, so a plugin panel looks
//! like the rest of the window and cannot choose colours, sizes or fonts: it only picks semantic styles and gap
//! tokens. Every interactive element is a tab stop with an accessible name. An element sends its event at once, except
//! a text input, which sends `Changed` on Enter or when it loses the focus (the host wires that through the input
//! entities it hands in).

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Entity, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Sizable as _};
use sqler_plugin_api::{
    ButtonStyle, Direction, Gap, Table, TextStyle, Widget, WidgetEvent, WidgetTree,
};

use crate::theme::SPACE;
use crate::workspace::chrome::ui_scale;

/// Where an event of a widget goes.
pub(crate) type Emit = Rc<dyn Fn(WidgetEvent, &mut Window, &mut App)>;

/// The width of a table column.
const CELL_WIDTH: f32 = 140.;
/// The most table cells (headings included) drawn for one tree.
pub(crate) const MAX_DRAWN_CELLS: usize = 5_000;
/// The tallest a table grows before it scrolls.
const TABLE_MAX_HEIGHT: f32 = 320.;

/// The gap between the children of a stack, as a space token.
fn gap_size(gap: Gap) -> Option<f32> {
    match gap {
        Gap::Small => Some(SPACE[1]),
        Gap::Medium => Some(SPACE[3]),
        Gap::Large => Some(SPACE[5]),
        Gap::None => None,
    }
}

/// The element id of a widget of the panel.
fn element_id(prefix: &str, id: &str) -> SharedString {
    SharedString::from(format!("plugin-widget-{prefix}-{id}"))
}

/// Draws `tree`. `inputs` holds the editor of every text input, by element id.
pub(crate) fn render_tree(
    tree: &WidgetTree,
    inputs: &HashMap<String, Entity<InputState>>,
    emit: &Emit,
    cx: &App,
) -> AnyElement {
    let budget = Cell::new(MAX_DRAWN_CELLS);
    node(tree, 0, inputs, emit, &budget, cx)
}

#[allow(clippy::too_many_lines)] // one arm per widget
fn node(
    tree: &WidgetTree,
    index: usize,
    inputs: &HashMap<String, Entity<InputState>>,
    emit: &Emit,
    budget: &Cell<usize>,
    cx: &App,
) -> AnyElement {
    let Some(n) = tree.nodes().get(index) else {
        return div().into_any_element();
    };
    let scale = ui_scale(cx);
    let theme = cx.theme();
    match &n.widget {
        Widget::Stack(stack) => {
            let mut el = div().flex().min_w_0();
            el = match stack.direction {
                Direction::Vertical => el.flex_col(),
                Direction::Horizontal => el.flex_row().flex_wrap().items_center(),
            };
            if let Some(gap) = gap_size(stack.gap) {
                el = el.gap(px(gap));
            }
            for child in &n.children {
                el = el.child(node(tree, *child as usize, inputs, emit, budget, cx));
            }
            el.into_any_element()
        }
        Widget::Text(text) => {
            let base = div().min_w_0().child(text.content.clone());
            match text.style {
                TextStyle::Muted => base
                    .text_size(px(scale.sm))
                    .text_color(theme.muted_foreground),
                TextStyle::Heading => base
                    .text_size(px(scale.lg))
                    .font_weight(FontWeight::SEMIBOLD),
                TextStyle::Code => base
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(scale.mono)),
                TextStyle::Body => base.text_size(px(scale.md)),
            }
            .into_any_element()
        }
        Widget::Button(button) => {
            let (id, event_id) = (element_id("button", &button.id), button.id.clone());
            let emit = Rc::clone(emit);
            let b = Button::new(id)
                .label(button.label.clone())
                .small()
                .tab_stop(true)
                .accessibility_label(button.label.clone())
                .on_click(move |_, window, cx| {
                    emit(WidgetEvent::Clicked(event_id.clone()), window, cx);
                });
            match button.style {
                ButtonStyle::Primary => b.primary(),
                ButtonStyle::Secondary => b.outline(),
            }
            .into_any_element()
        }
        Widget::TextInput(input) => match inputs.get(&input.id) {
            Some(state) => div()
                .min_w_0()
                .child(
                    Input::new(state).small().aria_label(
                        input
                            .placeholder
                            .clone()
                            .unwrap_or_else(|| input.id.clone()),
                    ),
                )
                .into_any_element(),
            None => div().into_any_element(),
        },
        Widget::Checkbox(check) => {
            let event_id = check.id.clone();
            let emit = Rc::clone(emit);
            Checkbox::new(element_id("check", &check.id))
                .label(check.label.clone())
                .accessibility_label(check.label.clone())
                .checked(check.checked)
                .on_click(move |checked, window, cx| {
                    let event = WidgetEvent::Toggled {
                        id: event_id.clone(),
                        checked: *checked,
                    };
                    emit(event, window, cx);
                })
                .into_any_element()
        }
        Widget::Select(select) => {
            // A short list of choices: one button per option, the chosen one filled.
            let mut row = div()
                .id(element_id("select", &select.id))
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(SPACE[1]))
                .role(Role::RadioGroup);
            for (i, option) in select.options.iter().enumerate() {
                let Ok(at) = u32::try_from(i) else {
                    break;
                };
                let (event_id, emit) = (select.id.clone(), Rc::clone(emit));
                let chosen = select.selected == Some(at);
                let b = Button::new(element_id(&format!("option{i}"), &select.id))
                    .label(option.clone())
                    .small()
                    .tab_stop(true)
                    .accessibility_label(option.clone())
                    .on_click(move |_, window, cx| {
                        let event = WidgetEvent::Selected {
                            id: event_id.clone(),
                            index: at,
                        };
                        emit(event, window, cx);
                    });
                row = row.child(if chosen { b.primary() } else { b.outline() });
            }
            row.into_any_element()
        }
        Widget::Table(table) => table_view(table, index, budget, cx),
        Widget::Divider => div()
            .w_full()
            .border_t_1()
            .border_color(theme.border)
            .into_any_element(),
    }
}

/// How many rows of a table fit in what is left of the budget, and so are drawn. Every column heading and every cell
/// costs one; the headings are always drawn.
fn rows_within(columns: usize, rows: usize, left: usize) -> usize {
    let after_head = left.saturating_sub(columns);
    after_head
        .checked_div(columns)
        .map_or(0, |fit| rows.min(fit))
}

/// A read-only table in a scroll container. At most what is left of `budget` in cells is drawn, the rest is named in
/// one muted line, so a huge table cannot stall a frame.
fn table_view(table: &Table, index: usize, budget: &Cell<usize>, cx: &App) -> AnyElement {
    let columns = table.columns.len();
    let theme = cx.theme();
    let (border, muted) = (theme.border, theme.muted_foreground);
    if columns > budget.get() {
        return div()
            .text_color(muted)
            .child("Table not shown: the panel has too many cells")
            .into_any_element();
    }
    let shown = rows_within(columns, table.rows.len(), budget.get());
    budget.set(budget.get().saturating_sub(columns * (shown + 1)));
    let hidden = table.rows.len() - shown;
    let size = ui_scale(cx).sm;
    let cell = |text: &str| {
        div()
            .w(px(CELL_WIDTH))
            .flex_none()
            .px(px(SPACE[3]))
            .py(px(SPACE[1]))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(text.to_owned())
    };
    let mut head = div()
        .flex()
        .flex_row()
        .border_b_1()
        .border_color(border)
        .text_color(muted)
        .font_weight(FontWeight::MEDIUM);
    for column in &table.columns {
        head = head.child(cell(column));
    }
    let mut body = div()
        .id(element_id("table", &index.to_string()))
        .role(Role::Table)
        .max_h(px(TABLE_MAX_HEIGHT))
        .overflow_scroll()
        .flex()
        .flex_col()
        .border_1()
        .border_color(border)
        .rounded(cx.theme().radius)
        .text_size(px(size))
        .child(head);
    for row in table.rows.iter().take(shown) {
        let mut line = div().flex().flex_row();
        for text in row {
            line = line.child(cell(text));
        }
        body = body.child(line);
    }
    if hidden > 0 {
        body = body.child(
            div()
                .px(px(SPACE[3]))
                .py(px(SPACE[1]))
                .text_color(muted)
                .child(format!("{hidden} more rows not shown")),
        );
    }
    body.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_tables_are_drawn_whole() {
        assert_eq!(rows_within(3, 10, MAX_DRAWN_CELLS), 10);
        assert_eq!(rows_within(0, 10, MAX_DRAWN_CELLS), 0);
    }

    #[test]
    fn the_worst_legal_table_is_cut_to_the_budget() {
        // 1000 rows by 50 columns is 50 000 cells.
        let shown = rows_within(50, 1000, MAX_DRAWN_CELLS);
        assert!(shown < 1000);
        assert!((shown + 1) * 50 <= MAX_DRAWN_CELLS);
    }

    #[test]
    fn many_tables_share_one_budget() {
        // 2000 tables of 1000 x 50: the budget runs out, and later tables are not drawn at all.
        let budget = Cell::new(MAX_DRAWN_CELLS);
        let mut drawn = 0;
        for _ in 0..2000 {
            if 50 > budget.get() {
                continue;
            }
            let shown = rows_within(50, 1000, budget.get());
            drawn += 50 * (shown + 1);
            budget.set(budget.get().saturating_sub(50 * (shown + 1)));
        }
        assert!(drawn <= MAX_DRAWN_CELLS);
    }
}
