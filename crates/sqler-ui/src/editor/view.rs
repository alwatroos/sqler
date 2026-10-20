// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The SQL editor views: one `gpui-component` code editor per editor tab.
//!
//! The editor state owns the text. The headless logic of this module's parent never sees it
//! directly: [`EditorViews::buffer`] copies an [`EditorBuffer`] out, and [`EditorViews::apply`]
//! carries out the effects that come back. Matching brackets are framed through a range
//! decoration collection; the index is rebuilt only when the text changed and the cursor touches a
//! bracket.

use std::collections::HashMap;

use gpui::{
    AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, Subscription, Window, div, relative,
};
use gpui_component::input::{Editor, EditorState, RangeDecoration, RangeDecorationCollection};
use sqler_sql::Dialect;

use super::{BracketIndex, EditorBuffer, EditorKey, touches_bracket};
use crate::tabs::TabId;
use crate::workspace::{Effect, Workspace, WorkspaceView};

/// The language name `gpui-component` highlights as SQL.
const LANGUAGE: &str = "sql";

/// The name of the editor's own "add cursor above" action.
const ADD_CURSOR_ABOVE: &str = "input::AddCursorAbove";
/// The name of the editor's own "add cursor below" action.
const ADD_CURSOR_BELOW: &str = "input::AddCursorBelow";

/// The brackets of the last text the cursor stood next to.
struct BracketCache {
    text: String,
    index: BracketIndex,
}

/// One editor tab.
struct EditorView {
    state: Entity<EditorState>,
    frames: RangeDecorationCollection,
    cache: Option<BracketCache>,
    /// Keeps the observer of the state alive.
    _observe: Subscription,
}

/// The editor tabs of the workspace.
#[derive(Default)]
pub(crate) struct EditorViews {
    views: HashMap<TabId, EditorView>,
}

impl EditorViews {
    /// Creates the editor of every editor tab that has none yet and focuses the new ones.
    pub(crate) fn materialize(
        &mut self,
        ws: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) {
        let wanted: Vec<TabId> = ws
            .tabs()
            .iter()
            .map(crate::tabs::Tab::id)
            .filter(|id| ws.editor_doc(*id).is_some() && !self.views.contains_key(id))
            .collect();
        for tab in wanted {
            let seed = ws.take_editor_seed(tab);
            let state = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(LANGUAGE)
                    .line_number(true)
            });
            if let Some(seed) = seed {
                state.update(cx, |s, cx| s.set_value(seed, window, cx));
            }
            let frames = state.update(cx, |s, cx| {
                s.create_range_decorations_collection(Vec::new(), cx)
            });
            let observe = cx.observe(&state, move |view, state, cx| {
                let dialect = view
                    .workspace()
                    .editor_doc(tab)
                    .and_then(super::EditorDoc::dialect);
                view.editors.refresh_brackets(tab, &state, dialect, cx);
            });
            let handle = state.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            self.views.insert(
                tab,
                EditorView {
                    state,
                    frames,
                    cache: None,
                    _observe: observe,
                },
            );
        }
    }

    /// Drops the editors of tabs that are gone.
    pub(crate) fn retain(&mut self, ws: &Workspace) {
        self.views.retain(|id, _| ws.editor_doc(*id).is_some());
    }

    /// Whether `tab` has an editor on screen.
    pub(crate) fn contains(&self, tab: TabId) -> bool {
        self.views.contains_key(&tab)
    }

    /// The element of an editor tab: the editor fills the tab body.
    pub(crate) fn body(&self, tab: TabId) -> Option<impl IntoElement + use<>> {
        let view = self.views.get(&tab)?;
        Some(
            div()
                .id("sql-editor")
                .flex_1()
                .min_h_0()
                .w_full()
                .child(Editor::new(&view.state).h(relative(1.))),
        )
    }

    /// Gives the editor of `tab` the keyboard focus.
    pub(crate) fn focus(&self, tab: TabId, window: &mut Window, cx: &mut Context<WorkspaceView>) {
        if let Some(view) = self.views.get(&tab) {
            let handle = view.state.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// A copy of the text, the active selection and the cursor of the editor of `tab`.
    pub(crate) fn buffer(&self, tab: TabId, cx: &gpui::App) -> Option<EditorBuffer> {
        let state = self.views.get(&tab)?.state.read(cx);
        Some(EditorBuffer {
            text: state.text().to_string(),
            selection: state.selected_range(),
            cursor: state.cursor(),
        })
    }

    /// Carries out an editor effect. Gives any other effect back to the caller.
    pub(crate) fn apply(
        &self,
        effect: Effect,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) -> Option<Effect> {
        match effect {
            Effect::EditorEdit { tab, edit } => {
                if let Some(view) = self.views.get(&tab) {
                    // One `replace` is one undo step: the old text comes back with a single undo.
                    view.state.update(cx, |s, cx| {
                        s.set_selected_range(edit.range.clone(), cx);
                        s.replace(edit.text.clone(), window, cx);
                        s.set_selected_range(edit.select_after.clone(), cx);
                    });
                }
                None
            }
            Effect::EditorSelect { tab, range } => {
                if let Some(view) = self.views.get(&tab) {
                    view.state
                        .update(cx, |s, cx| s.set_selected_range(range, cx));
                }
                None
            }
            Effect::EditorAddSelection { tab, range } => {
                if let Some(view) = self.views.get(&tab) {
                    view.state.update(cx, |s, cx| s.add_selection(range, cx));
                }
                None
            }
            Effect::EditorKey { tab, key } => {
                self.key(tab, key, window, cx);
                None
            }
            other => Some(other),
        }
    }

    /// Runs the editor's own action for a key, with the editor as its target.
    fn key(
        &self,
        tab: TabId,
        key: EditorKey,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) {
        let Some(view) = self.views.get(&tab) else {
            return;
        };
        let name = match key {
            EditorKey::AddCursorAbove => ADD_CURSOR_ABOVE,
            EditorKey::AddCursorBelow => ADD_CURSOR_BELOW,
        };
        match cx.build_action(name, None) {
            Ok(action) => {
                let handle = view.state.read(cx).focus_handle(cx);
                handle.dispatch_action(action.as_ref(), window, cx);
            }
            Err(err) => eprintln!("sqler: editor action {name}: {err}"),
        }
    }

    /// Frames the bracket pair the cursor touches, or clears the frames.
    fn refresh_brackets(
        &mut self,
        tab: TabId,
        state: &Entity<EditorState>,
        dialect: Option<Dialect>,
        cx: &mut Context<WorkspaceView>,
    ) {
        let Some(view) = self.views.get_mut(&tab) else {
            return;
        };
        let (touching, cursor) = {
            let s = state.read(cx);
            (cursor_touches_bracket(s), s.cursor())
        };
        let pair = if touching {
            let text = state.read(cx).text().to_string();
            if view.cache.as_ref().is_none_or(|c| c.text != text) {
                let index = BracketIndex::build(dialect, &text);
                view.cache = Some(BracketCache { text, index });
            }
            view.cache
                .as_ref()
                .and_then(|c| c.index.pair_at(&c.text, cursor))
        } else {
            None
        };
        let frames = match pair {
            Some((open, close)) => vec![
                RangeDecoration::new(open..open + 1),
                RangeDecoration::new(close..close + 1),
            ],
            None => Vec::new(),
        };
        view.frames.set(frames, cx);
    }
}

/// Whether the cursor of the editor stands right before or after a bracket. Reads the two bytes
/// around the cursor only (brackets are ASCII), so it costs the same for any text size.
fn cursor_touches_bracket(state: &EditorState) -> bool {
    let rope = state.text();
    let at = state.cursor().min(rope.len());
    let around = [
        at.checked_sub(1).map(|i| rope.byte(i)),
        (at < rope.len()).then(|| rope.byte(at)),
    ];
    window_touches_bracket(around[0], around[1])
}

/// Whether the byte before or the byte after the cursor is a bracket. A non-ASCII byte is never
/// one, and must not widen into a multibyte char that shifts the cursor offset.
fn window_touches_bracket(before: Option<u8>, after: Option<u8>) -> bool {
    let window: String = [before, after]
        .into_iter()
        .flatten()
        .map(|b| if b.is_ascii() { char::from(b) } else { ' ' })
        .collect();
    // The window holds the byte before the cursor, then the byte after it.
    touches_bracket(&window, usize::from(before.is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multibyte_char_before_a_bracket_still_touches_it() {
        let text = "zażółć(";
        let at = text.len() - 1;
        let bytes = text.as_bytes();
        assert!(window_touches_bracket(
            at.checked_sub(1).map(|i| bytes[i]),
            bytes.get(at).copied()
        ));
        // A multibyte char on its own touches nothing.
        let ends = "zażółć";
        let b = ends.as_bytes();
        assert!(!window_touches_bracket(b.last().copied(), None));
        assert!(window_touches_bracket(None, Some(b'(')));
        assert!(!window_touches_bracket(None, None));
    }
}
