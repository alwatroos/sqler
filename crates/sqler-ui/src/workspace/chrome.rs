// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Shared chrome of the component views: the size constants of the workbench design
//! (`docs/src/architecture/workbench.md` section 3), list-row states, the dialog frame and a menu
//! item with a shortcut hint. Colours come from the active theme, spacing from [`SPACE`] and
//! text sizes from [`ui_scale`]; views use these instead of literals.

use gpui::{
    App, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{ActiveTheme as _, Icon};

use crate::theme::icons::AppIcon;
use crate::theme::{SPACE, TypeScale, type_scale};

/// Height of a list row, in logical pixels.
pub(crate) const ROW_HEIGHT: f32 = 24.;
/// Height of a toolbar.
pub(crate) const TOOLBAR_HEIGHT: f32 = 32.;
/// Height of a panel header.
pub(crate) const HEADER_HEIGHT: f32 = 30.;
/// Small icon size.
pub(crate) const ICON_SM: f32 = 14.;
/// Medium icon size.
pub(crate) const ICON_MD: f32 = 16.;
/// Indent per tree level.
pub(crate) const INDENT: f32 = 12.;
/// Width of the label column of a form row.
pub(crate) const LABEL_WIDTH: f32 = 120.;
/// Width of a small dialog.
pub(crate) const DIALOG_W_SM: f32 = 460.;
/// Width of a medium dialog.
pub(crate) const DIALOG_W_MD: f32 = 560.;
/// Width of a large dialog.
pub(crate) const DIALOG_W_LG: f32 = 640.;
/// Maximum height of a dialog.
pub(crate) const DIALOG_MAX_H: f32 = 720.;
/// Maximum height of a list inside a dialog.
pub(crate) const LIST_MAX_H: f32 = 360.;
/// Width of the palette and picker overlays.
pub(crate) const OVERLAY_W: f32 = 600.;
/// Distance of the overlays from the top of the window.
pub(crate) const OVERLAY_TOP: f32 = 48.;
/// Height of an overlay row.
pub(crate) const OVERLAY_ROW: f32 = 28.;
/// Width of the focus indicator on the left edge of a row.
const FOCUS_EDGE: f32 = 2.;
/// Opacity of a disabled row.
const DISABLED_OPACITY: f32 = 0.5;
/// Font weight of a dialog title.
const TITLE_WEIGHT: f32 = 600.;

/// What a list row is currently like.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowState {
    /// The row is the selected one.
    pub selected: bool,
    /// The list has the keyboard focus.
    pub focused: bool,
    /// The row cannot be used.
    pub disabled: bool,
}

/// How a row is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowLook {
    /// Not selected; hover and press feedback.
    Plain,
    /// Selected, the list has no focus.
    SelectedUnfocused,
    /// Selected, the list has the focus.
    SelectedFocused,
    /// Greyed out.
    Disabled,
}

/// The look of a row in `s`.
pub(crate) fn row_look(s: RowState) -> RowLook {
    if s.disabled {
        RowLook::Disabled
    } else if s.selected && s.focused {
        RowLook::SelectedFocused
    } else if s.selected {
        RowLook::SelectedUnfocused
    } else {
        RowLook::Plain
    }
}

/// The type scale for the current UI font size.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 1..=u16::MAX
pub(crate) fn ui_scale(cx: &App) -> TypeScale {
    let size: f32 = cx.theme().font_size.into();
    type_scale(size.round().clamp(1., f32::from(u16::MAX)) as u16)
}

/// A list row: fixed height, a left edge that shows the focus, and the states of [`RowLook`].
/// The caller adds the children and the click handler.
pub(crate) fn list_row(id: impl Into<ElementId>, s: RowState, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let (hover, active, edge) = (
        theme.list_hover,
        theme.list_active,
        theme.list_active_border,
    );
    let row = div()
        .id(id)
        .h(px(ROW_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .px(px(SPACE[3]))
        .gap(px(SPACE[2]))
        .rounded(theme.radius)
        .border_l(px(FOCUS_EDGE))
        .border_color(gpui::transparent_black());
    match row_look(s) {
        RowLook::Plain => row
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(active)),
        RowLook::SelectedFocused => row.cursor_pointer().bg(active).border_color(edge),
        // The strong fill without the edge: distinct from hover, so the selection stays
        // visible under the pointer; the edge is what marks the focus.
        RowLook::SelectedUnfocused => row.cursor_pointer().bg(active),
        RowLook::Disabled => row
            .opacity(DISABLED_OPACITY)
            .text_color(theme.muted_foreground),
    }
}

/// The dimmed layer behind a dialog; it takes the mouse and centres its child.
pub(crate) fn scrim(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(cx.theme().overlay)
        .occlude()
}

/// The frame of a dialog: popover colours, a border, rounded corners and a column of children.
pub(crate) fn dialog_panel(id: impl Into<ElementId>, width: f32, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .w(px(width))
        .max_h(px(DIALOG_MAX_H))
        .flex()
        .flex_col()
        .gap(px(SPACE[4]))
        .px(px(SPACE[6]))
        .py(px(SPACE[5]))
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
}

/// The title row of a dialog: an optional 16 px icon and the text.
pub(crate) fn dialog_title(icon: Option<AppIcon>, text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    let title = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[3]))
        .text_size(px(ui_scale(cx).lg))
        .font_weight(FontWeight(TITLE_WEIGHT));
    let title = match icon {
        Some(icon) => title.child(
            Icon::new(icon)
                .size(px(ICON_MD))
                .text_color(theme.muted_foreground),
        ),
        None => title,
    };
    title.child(text.into())
}

/// The button row at the bottom of a dialog; the caller appends the buttons.
pub(crate) fn dialog_footer(cx: &App) -> Div {
    div()
        .flex()
        .flex_row()
        .justify_end()
        .gap(px(SPACE[3]))
        .pt(px(SPACE[4]))
        .border_t_1()
        .border_color(cx.theme().border)
}

/// A form row: the label in a fixed, muted column and the content next to it.
pub(crate) fn field_row(
    label: impl Into<SharedString>,
    content: impl IntoElement,
    cx: &App,
) -> Div {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(SPACE[3]))
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_none()
                // Aligns the label with the first line of a control.
                .pt(px(SPACE[2]))
                .text_size(px(ui_scale(cx).sm))
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        // `min_w_0` lets long text in the content column wrap instead of widening the row.
        .child(div().flex_1().min_w_0().child(content))
}

/// A menu item with an optional icon and, right-aligned, its shortcut hint.
pub(crate) fn hinted_item(
    label: impl Into<SharedString>,
    icon: Option<AppIcon>,
    hint: Option<SharedString>,
) -> PopupMenuItem {
    let label: SharedString = label.into();
    let item = PopupMenuItem::element(move |_, cx| {
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(px(SPACE[5]))
            .child(label.clone())
            .child(div().flex_1());
        if let Some(hint) = &hint {
            row = row.child(
                div()
                    .text_size(px(ui_scale(cx).xs))
                    .text_color(cx.theme().muted_foreground)
                    .child(hint.clone()),
            );
        }
        row
    });
    match icon {
        Some(icon) => item.icon(icon),
        None => item,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, Window};

    use super::*;
    use crate::theme::Appearance;
    use crate::theme::apply::apply;

    fn state(selected: bool, focused: bool, disabled: bool) -> RowState {
        RowState {
            selected,
            focused,
            disabled,
        }
    }

    #[test]
    fn row_look_table() {
        let table = [
            (state(false, false, false), RowLook::Plain),
            (state(false, true, false), RowLook::Plain),
            (state(true, false, false), RowLook::SelectedUnfocused),
            (state(true, true, false), RowLook::SelectedFocused),
            (state(false, false, true), RowLook::Disabled),
            (state(true, true, true), RowLook::Disabled),
        ];
        for (s, look) in table {
            assert_eq!(row_look(s), look, "{s:?}");
        }
    }

    struct Gallery;

    impl Render for Gallery {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let rows = [
                state(false, false, false),
                state(true, false, false),
                state(true, true, false),
                state(false, false, true),
            ];
            let mut col = div().flex().flex_col();
            for (i, s) in rows.into_iter().enumerate() {
                col = col.child(
                    list_row(("row", i), s, cx)
                        .debug_selector(|| format!("row-{i}"))
                        .child("row"),
                );
            }
            scrim("scrim", cx).child(
                dialog_panel("panel", DIALOG_W_SM, cx)
                    .debug_selector(|| "panel".to_owned())
                    .child(dialog_title(Some(AppIcon::TriangleAlert), "Title", cx))
                    .child(field_row("Label", "content", cx))
                    .child(col)
                    .child(dialog_footer(cx).child("Ok")),
            )
        }
    }

    #[gpui::test]
    fn gallery_renders_in_both_themes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
        });
        let (_gallery, cx) = cx.add_window_view(|_, _| Gallery);
        for look in [Appearance::Dark, Appearance::Light] {
            cx.update(|_, cx| apply(look, 14, cx));
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            for selector in ["panel", "row-0", "row-1", "row-2", "row-3"] {
                let bounds = cx.debug_bounds(selector);
                let size = bounds.map(|b| b.size);
                assert!(
                    size.is_some_and(|s| s.width > px(0.) && s.height > px(0.)),
                    "{selector} in {look:?}: {size:?}"
                );
            }
        }
    }
}
