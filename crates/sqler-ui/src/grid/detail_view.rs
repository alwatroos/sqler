// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The cell detail pane: the full value of the active cell, to the right of the results table.
//!
//! The pane is its own entity. It observes the table and re-renders itself only when the active
//! cell changes, so scrolling and ingestion never rebuild the workspace for it. The text is
//! formatted by the headless [`CellDetail`]; this file only draws lines by index and moves
//! values between the model, the background executor and the clipboard effect.
//!
//! Values may hold secrets: nothing here logs a value, and no type prints one in `Debug`.

use std::fmt;
use std::ops::Range;

use gpui::{
    App, Context, EventEmitter, Global, InteractiveElement as _, IntoElement,
    ListHorizontalSizingBehavior, ParentElement as _, Render, Styled as _, Subscription, Task,
    UniformListScrollHandle, WeakEntity, Window, div, px, uniform_list,
};
use gpui_component::button::Button;
use gpui_component::table::TableState;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};

use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::{section_header, toolbar_button};
use crate::workspace::chrome::{HEADER_HEIGHT, ui_scale};

use super::view::{GridDelegate, Table};
use super::{CellDetail, CellPos, DetailCap, DetailKind, copy_text};

/// Height of one body row, in pixels.
const ROW_HEIGHT: f32 = 18.;
/// Width of the gutter that marks continued lines, in pixels.
const GUTTER_WIDTH: f32 = 14.;

/// Whether the pane is open. One value for the whole session, so it stays open across tabs and
/// query reruns. It starts closed and is not persisted.
#[derive(Default)]
pub(crate) struct DetailPaneState {
    /// Whether the pane is shown next to the results table.
    pub(crate) open: bool,
}

impl Global for DetailPaneState {}

/// Opens the pane if it is closed, closes it if it is open.
pub(crate) fn toggle(cx: &mut App) {
    let state = cx.default_global::<DetailPaneState>();
    state.open = !state.open;
}

/// Whether the pane is open.
pub(crate) fn is_open(cx: &App) -> bool {
    cx.try_global::<DetailPaneState>().is_some_and(|s| s.open)
}

/// The text a Copy click puts on the clipboard.
pub(crate) struct CopyCellValue(pub(crate) String);

/// Prints the size, never the value.
impl fmt::Debug for CopyCellValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CopyCellValue({} bytes)", self.0.len())
    }
}

/// What the pane is about: the active cell (model coordinates) and the column count, so a
/// result whose columns changed is not mistaken for the same cell.
type Key = (Option<CellPos>, usize);

/// The detail pane of one results table.
pub(crate) struct CellDetailView {
    table: WeakEntity<TableState<GridDelegate>>,
    /// The key last seen by the observer, to notify only on a change.
    key: Key,
    /// The key `doc` was built for.
    doc_key: Option<Key>,
    doc: Option<CellDetail>,
    /// The running Show all job. Dropping it cancels the wait for its result.
    task: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    _observe: Subscription,
}

impl EventEmitter<CopyCellValue> for CellDetailView {}

impl CellDetailView {
    /// A pane for `table`. It draws nothing of its own until it is placed next to the table.
    pub(crate) fn new(table: &Table, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(table, |this, table, cx| {
            if !is_open(cx) {
                return;
            }
            let key = Self::key_of(&table, cx);
            if key != this.key {
                this.key = key;
                cx.notify();
            }
        });
        Self {
            table: table.downgrade(),
            key: Self::key_of(table, cx),
            doc_key: None,
            doc: None,
            task: None,
            scroll: UniformListScrollHandle::new(),
            _observe: observe,
        }
    }

    fn key_of(table: &Table, cx: &App) -> Key {
        let state = table.read(cx).delegate();
        (state.active_cell(), state.grid().column_count())
    }

    /// Formats the active cell, bounded.
    fn build(&self, key: Key, cx: &App) -> Option<CellDetail> {
        let table = self.table.upgrade()?;
        let grid = table.read(cx).delegate().grid();
        let CellPos { row, col } = key.0?;
        let header = grid.header(col)?;
        let cell = grid.cell(row, col)?;
        Some(CellDetail::build(header, cell, DetailCap::Bounded))
    }

    /// Rebuilds the detail when the active cell changed.
    fn refresh(&mut self, cx: &App) {
        let Some(table) = self.table.upgrade() else {
            return;
        };
        let key = Self::key_of(&table, cx);
        self.key = key;
        if self.doc_key == Some(key) {
            return;
        }
        // A job for the previous cell must not land on this one.
        self.task = None;
        self.doc = self.build(key, cx);
        self.doc_key = Some(key);
        self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
    }

    /// Copies the value of the shown cell, as stored.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.table.upgrade() else {
            return;
        };
        let text = {
            let grid = table.read(cx).delegate().grid();
            let Some(CellPos { row, col }) = self.key.0 else {
                return;
            };
            grid.cell(row, col).and_then(copy_text)
        };
        if let Some(text) = text {
            cx.emit(CopyCellValue(text));
        }
    }

    /// Rebuilds the detail without caps on the background executor.
    fn show_all(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.table.upgrade() else {
            return;
        };
        let key = self.key;
        let Some(CellPos { row, col }) = key.0 else {
            return;
        };
        let (header, value) = {
            let grid = table.read(cx).delegate().grid();
            let (Some(header), Some(cell)) = (grid.header(col), grid.cell(row, col)) else {
                return;
            };
            (header.clone(), cell.to_value())
        };
        self.task = Some(cx.spawn(async move |this, cx| {
            let detail = cx
                .background_executor()
                .spawn(async move { CellDetail::build_owned(&header, &value, DetailCap::Full) })
                .await;
            this.update(cx, |view, cx| {
                if view.doc_key == Some(key) {
                    view.doc = Some(detail);
                    view.task = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for CellDetailView {
    #[allow(clippy::too_many_lines)] // one declarative element tree
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh(cx);
        let theme = cx.theme();
        let (muted, border, bg) = (theme.muted_foreground, theme.border, theme.background);
        let mono = theme.mono_font_family.clone();
        let scale = ui_scale(cx);
        let pane = v_flex()
            .id("cell-detail")
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(bg)
            .border_l_1()
            .border_color(border);
        let Some(doc) = self.doc.as_ref() else {
            return pane
                .p(px(SPACE[3]))
                .text_size(px(scale.sm))
                .text_color(muted)
                .child("Select a cell to see its full value.");
        };
        let is_null = doc.kind() == DetailKind::Null;
        let loading = self.task.is_some();
        let header = section_header("Cell value", cx)
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .border_b_1()
            .border_color(border);
        let identity = v_flex()
            .px(px(SPACE[3]))
            .py(px(SPACE[2]))
            .gap(px(SPACE[1]))
            .flex_none()
            .child(
                h_flex()
                    .gap(px(SPACE[2]))
                    .items_baseline()
                    .child(
                        div()
                            .text_size(px(scale.sm))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(doc.column().to_owned()),
                    )
                    .child(
                        div()
                            .text_size(px(scale.xs))
                            .text_color(muted)
                            .child(doc.type_label()),
                    ),
            )
            .child(
                div()
                    .text_size(px(scale.sm))
                    .text_color(muted)
                    .child(doc.summary()),
            );
        let toolbar = h_flex()
            .px(px(SPACE[3]))
            .pb(px(SPACE[2]))
            .gap(px(SPACE[2]))
            .flex_none()
            .child(
                toolbar_button(
                    "cell-detail-copy",
                    AppIcon::Copy,
                    "Copy value",
                    "Copy value",
                )
                .disabled(is_null)
                .on_click(cx.listener(|view, _, _, cx| view.copy(cx))),
            )
            .children(doc.is_truncated().then(|| {
                Button::new("cell-detail-show-all")
                    .label(if loading { "Loading…" } else { "Show all" })
                    .small()
                    .disabled(loading)
                    .on_click(cx.listener(|view, _, _, cx| view.show_all(cx)))
            }));
        let notes = v_flex()
            .px(px(SPACE[3]))
            .pb(px(SPACE[2]))
            .flex_none()
            .children(doc.notes().iter().map(|n| {
                div()
                    .text_size(px(scale.sm))
                    .text_color(muted)
                    .child(n.clone())
            }));
        let lines = doc.line_count();
        // The scroll range takes its width from one measured line: the widest, not the first.
        let widest = doc.widest_line();
        let body = uniform_list(
            "cell-detail-lines",
            lines,
            cx.processor(move |view, range: Range<usize>, _, cx| {
                let muted = cx.theme().muted_foreground;
                let mono = cx.theme().mono_font_family.clone();
                let mono_size = px(ui_scale(cx).mono);
                let Some(doc) = view.doc.as_ref() else {
                    return Vec::new();
                };
                range
                    .filter_map(|ix| doc.line(ix).map(|line| (ix, line)))
                    .map(|(ix, line)| {
                        h_flex()
                            .id(("cell-detail-line", ix))
                            .h(px(ROW_HEIGHT))
                            .flex_none()
                            .font_family(mono.clone())
                            .text_size(mono_size)
                            .whitespace_nowrap()
                            .child(
                                div()
                                    .w(px(GUTTER_WIDTH))
                                    .flex_none()
                                    .text_color(muted)
                                    .child(if line.continued { "↪" } else { "" }),
                            )
                            .child(line.text.into_owned())
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(widest)
        .flex_1()
        .min_h_0()
        .w_full()
        .px(px(SPACE[3]))
        .font_family(mono);
        pane.child(header)
            .child(identity)
            .child(toolbar)
            .child(notes)
            .child(body)
    }
}
