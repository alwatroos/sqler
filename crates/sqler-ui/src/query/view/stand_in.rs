// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plain multi-line input that stands in for the SQL editor until T9 replaces it.

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement as _, Styled as _, Window, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Textarea, TextareaState};

use crate::query::BufferSnapshot;
use crate::workspace::WorkspaceView;

/// A new, empty stand-in input.
pub(super) fn new_editor(
    window: &mut Window,
    cx: &mut Context<WorkspaceView>,
) -> Entity<TextareaState> {
    cx.new(|cx| TextareaState::new(window, cx).placeholder("Type SQL here"))
}

/// Reads text, selection and cursor of the input. Offsets are clamped to the text and moved back
/// onto a character boundary.
pub(super) fn snapshot(editor: &Entity<TextareaState>, cx: &App) -> BufferSnapshot {
    let state = editor.read(cx);
    let text = state.value().to_string();
    let fit = |offset: usize| {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    let selected = state.selected_range();
    let (start, end) = (fit(selected.start), fit(selected.end));
    let cursor = fit(state.cursor());
    BufferSnapshot {
        text,
        selection: start.min(end)..start.max(end),
        cursor,
    }
}

/// The stand-in input as an element, in the monospace font.
pub(crate) fn editor_element(
    editor: &Entity<TextareaState>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let mono = cx.theme().mono_font_family.clone();
    Textarea::new(editor)
        .h(px(220.))
        .font_family(mono)
        .appearance(false)
        .bordered(false)
        .into_any_element()
}
