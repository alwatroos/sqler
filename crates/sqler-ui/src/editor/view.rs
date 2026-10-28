// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The SQL editor views: one `gpui-component` code editor per editor tab.
//!
//! The editor state owns the text. The headless logic of this module's parent never sees it
//! directly: [`EditorViews::buffer`] copies an [`EditorBuffer`] out, and [`EditorViews::apply`]
//! carries out the effects that come back. Matching brackets are framed through a range
//! decoration collection; the index is rebuilt only when the text changed and the cursor touches a
//! bracket.

mod provider;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    KeyBinding, NoAction, ParentElement as _, Styled as _, Subscription, Task, Window, div,
};
use gpui_component::input::{
    Editor, EditorState, InputEvent, RangeDecoration, RangeDecorationCollection,
};
use sqler_sql::Dialect;

use self::provider::{IntelHandle, SqlProvider};
use super::intellisense::{DIAGNOSE_DEBOUNCE, bootstrap_keys};
use super::{BracketIndex, EditorBuffer, EditorKey, touches_bracket};
use crate::tabs::TabId;
use crate::workspace::{Effect, Workspace, WorkspaceView};

/// The language name `gpui-component` highlights as SQL.
const LANGUAGE: &str = "sql";

/// The name of the editor's own "add cursor above" action.
const ADD_CURSOR_ABOVE: &str = "input::AddCursorAbove";
/// The name of the editor's own "add cursor below" action.
const ADD_CURSOR_BELOW: &str = "input::AddCursorBelow";
/// The name of the editor's own "show completions" action.
const SHOW_COMPLETIONS: &str = "input::ShowCompletions";

/// The key context of the editor widget.
const INPUT_CONTEXT: &str = "Input";

/// Unbinds the editor's own `secondary-enter` and `secondary-.` bindings, so Cmd/Ctrl+Enter
/// (run) and Cmd/Ctrl+. (`query:cancel`) reach the workspace.
///
/// GPUI runs key bindings before the capture listeners of ancestors. `gpui-base` binds
/// `secondary-enter` in the `Input` context to `Enter { secondary: true }`, which in a multi-line
/// editor inserts a newline over the selection and stops the key. It binds `secondary-.` to
/// `ToggleCodeActions`, which also stops the key although the app registers no code-action
/// provider, so the chord never cancelled a running query. Nothing else is unbound.
pub(crate) fn init_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-enter", NoAction, Some(INPUT_CONTEXT)),
        KeyBinding::new("secondary-.", NoAction, Some(INPUT_CONTEXT)),
    ]);
}

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
    /// What the completion and hover providers and the diagnose task share.
    intel: IntelHandle,
    /// The pending or running diagnose pass; a newer one replaces it.
    diagnose: Task<()>,
    /// Keeps the observer of the state alive.
    _observe: Subscription,
    /// Keeps the change subscription alive.
    _change: Subscription,
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
            let intel: IntelHandle = Rc::new(RefCell::new(provider::IntelState::default()));
            SqlProvider::install(&state, &intel, cx);
            let shared = intel.clone();
            let change = cx.subscribe_in(
                &state,
                window,
                move |view, state, event: &InputEvent, window, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    shared.borrow_mut().intel.edited();
                    if !view.workspace().tab_decor(tab).is_some_and(|d| d.dirty) {
                        // Only the first edit of a tab costs anything.
                        view.editor_changed(tab, cx);
                    }
                    if let Some(v) = view.editors.views.get_mut(&tab) {
                        v.diagnose =
                            provider::diagnose_task(state, &shared, DIAGNOSE_DEBOUNCE, window, cx);
                    }
                },
            );
            let handle = state.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            self.views.insert(
                tab,
                EditorView {
                    state,
                    frames,
                    cache: None,
                    intel,
                    diagnose: Task::ready(()),
                    _observe: observe,
                    _change: change,
                },
            );
        }
        self.sync_intellisense(ws, window, cx);
    }

    /// Hands every editor the catalog and dialect of its connection. When the connection changed,
    /// the editor loads the schema listings and diagnoses; when the catalog has a newer snapshot
    /// (a refresh, a load), it diagnoses again.
    fn sync_intellisense(
        &mut self,
        ws: &Workspace,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) {
        for (tab, view) in &mut self.views {
            let ctx = ws
                .editor_doc(*tab)
                .and_then(super::EditorDoc::dialect)
                .zip(ws.editor_catalog(*tab))
                .map(|(dialect, (generation, catalog))| (generation, catalog, dialect));
            let (changed, preload, refreshed) = {
                let mut shared = view.intel.borrow_mut();
                let changed = shared.intel.bind(ctx.as_ref().map(|c| c.0));
                shared.ctx = ctx.as_ref().map(|(_, catalog, dialect)| provider::Context {
                    catalog: catalog.clone(),
                    dialect: *dialect,
                    plugins: ws.plugins().backend().cloned(),
                    driver: ws
                        .tabs()
                        .get(*tab)
                        .map(|t| t.connection().driver.as_str().to_owned()),
                });
                let (preload, refreshed) = match &ctx {
                    Some(_) if changed => (shared.intel.fresh(bootstrap_keys()), false),
                    Some((_, catalog, _)) => (
                        Vec::new(),
                        shared
                            .intel
                            .snapshot_changed(catalog.snapshot().generation()),
                    ),
                    None => (Vec::new(), false),
                };
                (changed, preload, refreshed)
            };
            if !preload.is_empty()
                && let Some((_, catalog, _)) = &ctx
            {
                // Its own task: the keys are marked asked, so a diagnose pass that is replaced
                // must not be the one that loses them.
                provider::preload(
                    &view.intel,
                    &view.state.downgrade(),
                    catalog,
                    preload,
                    window,
                    cx,
                );
            }
            if changed || refreshed {
                view.diagnose =
                    provider::diagnose_task(&view.state, &view.intel, Duration::ZERO, window, cx);
            }
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
                .flex()
                .flex_col()
                .child(Editor::new(&view.state).flex_1().min_h_0().w_full()),
        )
    }

    /// The laid-out bounds of the editor of `tab`, as of its last prepaint (tests only).
    #[cfg(test)]
    pub(crate) fn input_bounds(
        &self,
        tab: TabId,
        cx: &gpui::App,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        Some(self.views.get(&tab)?.state.read(cx).input_bounds())
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
            EditorKey::ShowCompletions => SHOW_COMPLETIONS,
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
        set_frames(&view.frames, pair, cx);
    }
}

/// Frames `pair`, or clears the frames. Setting the frames notifies the state, which runs the
/// bracket observer again, so frames that already hold the wanted ranges are left alone. The
/// collection is compared, not the last pair set: the ranges are anchored and an edit moves and
/// stretches them, so the same pair offsets can sit on deformed frames.
fn set_frames(
    frames: &RangeDecorationCollection,
    pair: Option<(usize, usize)>,
    cx: &mut gpui::App,
) {
    let want: Vec<std::ops::Range<usize>> = match pair {
        Some((open, close)) => vec![open..open + 1, close..close + 1],
        None => Vec::new(),
    };
    if frames.get_ranges(cx) == want {
        return;
    }
    frames.set(want.into_iter().map(RangeDecoration::new).collect(), cx);
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

    #[gpui::test]
    fn an_edit_across_a_framed_pair_resets_the_frames(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx)
                .language(LANGUAGE)
                .line_number(true)
        });
        let frames = state.update(cx, |s, cx| {
            s.create_range_decorations_collection(Vec::new(), cx)
        });
        let frame = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| frames.get_ranges(cx));
        let two = vec![1..2, 3..4];
        cx.update(|window, cx| state.update(cx, |s, cx| s.set_value("f(x)", window, cx)));
        cx.update(|_, cx| set_frames(&frames, Some((1, 3)), cx));
        assert_eq!(frame(cx), two);
        // Replacing `(x)` with `(y)` stretches both anchored frames over the new text.
        cx.update(|window, cx| {
            state.update(cx, |s, cx| {
                s.set_value("f(y)", window, cx);
            });
        });
        assert_ne!(frame(cx), two, "the edit deformed the anchored frames");
        cx.update(|_, cx| set_frames(&frames, Some((1, 3)), cx));
        assert_eq!(frame(cx), two);
        // The same pair again changes nothing.
        cx.update(|_, cx| set_frames(&frames, Some((1, 3)), cx));
        assert_eq!(frame(cx), two);
        cx.update(|_, cx| set_frames(&frames, None, cx));
        assert_eq!(frame(cx), Vec::<std::ops::Range<usize>>::new());
    }

    /// A window with the editor inside a root that records the chords reaching its capture phase.
    struct Host {
        state: Entity<EditorState>,
        seen: Rc<RefCell<Vec<String>>>,
    }

    impl gpui::Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let seen = Rc::clone(&self.seen);
            div()
                .size_full()
                .capture_key_down(move |event, _, _| {
                    seen.borrow_mut().push(event.keystroke.unparse());
                })
                .child(Editor::new(&self.state))
        }
    }

    const SCRIPT: &str = "select 1;\nselect 2;";

    /// Presses each chord with `0..8` selected; returns the text, selection and recorded chords
    /// after each press.
    fn press_all(
        cx: &mut gpui::TestAppContext,
        bind: bool,
        chords: &[&str],
    ) -> Vec<(String, std::ops::Range<usize>, usize)> {
        cx.update(|cx| {
            gpui_component::init(cx);
            if bind {
                init_keys(cx);
            }
        });
        let seen = Rc::new(RefCell::new(Vec::new()));
        let host_seen = Rc::clone(&seen);
        let (host, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(LANGUAGE)
                    .line_number(true)
            });
            Host {
                state,
                seen: host_seen,
            }
        });
        let state = host.read_with(cx, |h, _| h.state.clone());
        cx.update(|window, cx| {
            state.update(cx, |s, cx| s.set_value(SCRIPT, window, cx));
            state.update(cx, |s, cx| s.set_selected_range(0..8, cx));
            let handle = state.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        });
        let mut out = Vec::new();
        for chord in chords {
            cx.update(|_, cx| state.update(cx, |s, cx| s.set_selected_range(0..8, cx)));
            let before = seen.borrow().len();
            cx.simulate_keystrokes(chord);
            let reached = seen.borrow().len() - before;
            let (text, selection) = cx.update(|_, cx| {
                let s = state.read(cx);
                (s.text().to_string(), s.selected_range())
            });
            out.push((text, selection, reached));
        }
        out
    }

    /// The reproduction of the bug: without the unbinding the chord replaces the selection.
    #[gpui::test]
    fn without_init_keys_cmd_enter_replaces_the_selection(cx: &mut gpui::TestAppContext) {
        let r = press_all(cx, false, &["secondary-enter"]);
        assert_ne!(
            r[0].0, SCRIPT,
            "the editor swallowed the chord and edited the text"
        );
    }

    /// The reproduction of the cancel bug: without the unbinding the editor swallows the chord.
    #[gpui::test]
    fn without_init_keys_cmd_dot_is_swallowed(cx: &mut gpui::TestAppContext) {
        let r = press_all(cx, false, &["secondary-."]);
        assert_eq!(
            r[0].2, 0,
            "the editor's code-action binding swallowed the chord"
        );
    }

    #[gpui::test]
    fn run_and_toggle_chords_reach_the_root_and_leave_the_text(cx: &mut gpui::TestAppContext) {
        let chords = [
            "secondary-e",
            "secondary-enter",
            "secondary-shift-enter",
            "secondary-/",
            "secondary-.",
            "shift-alt-a",
        ];
        for (chord, (text, selection, reached)) in chords.iter().zip(press_all(cx, true, &chords)) {
            assert_eq!(text, SCRIPT, "{chord} changed the text");
            assert_eq!(selection, 0..8, "{chord} changed the selection");
            assert_eq!(reached, 1, "{chord} did not reach the root listener");
        }
    }
}
