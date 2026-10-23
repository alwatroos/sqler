// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The GUI side of the editor intellisense: the completion and hover providers of one editor and
//! the diagnose task.
//!
//! All the logic is in [`crate::editor::intellisense`]; this module moves it onto GPUI's background
//! executor, converts the answers to the LSP types `gpui-component` speaks, and awaits the catalog
//! loads on the foreground executor (the catalog worker itself runs on tokio).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, AsyncWindowContext, Entity, Focusable as _, Result, Task, WeakEntity,
    Window,
};
use gpui_component::input::{CompletionProvider, EditorState, HoverProvider, Rope, RopeExt as _};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Diagnostic, DiagnosticSeverity, Hover, HoverContents, MarkupContent, MarkupKind,
    NumberOrString, TextEdit,
};
use sqler_catalog::{CacheKey, Catalog};
use sqler_completion::{ItemKind, Severity, diagnose};
use sqler_sql::Dialect;

use sqler_plugin_api::CompletionKind;

use crate::editor::intellisense::{
    CatalogSchema, PREFETCH_LIMIT, PluginAnswers, TabIntel, code_name, complete_popup,
    hover_markdown, is_completion_trigger, load, merge_plugin_items, plugin_request,
    prefetch_keys_skipping,
};
use crate::plugins::PluginsBackend;
use crate::plugins::backend::PluginCompletions;

/// What the providers of one editor read: the connection's catalog and dialect, and the plugin
/// backend with the connection's driver id when completion plugins may add rows.
#[derive(Clone)]
pub(super) struct Context {
    pub(super) catalog: Catalog,
    pub(super) dialect: Dialect,
    pub(super) plugins: Option<Arc<dyn PluginsBackend>>,
    pub(super) driver: Option<String>,
}

/// The state shared by the providers, the diagnose task and the view of one editor.
#[derive(Default)]
pub(super) struct IntelState {
    pub(super) intel: TabIntel,
    pub(super) ctx: Option<Context>,
    /// The latest completion request, whatever load it waits for.
    request: Option<Request>,
    /// The plugin ask for the latest place, with the answers that arrived for it.
    plugin: Option<PluginAsk>,
    /// The number of the last plugin ask.
    next_ask: u64,
}

/// The plugins asked for one place, and what they answered. A new place replaces it.
struct PluginAsk {
    text: Rope,
    offset: usize,
    answers: PluginAnswers,
}

/// What the popup does about plugins at one place.
enum PluginPlan {
    /// No backend: built-in rows only.
    Off,
    /// This place was asked already: merge what arrived, ask nobody.
    Cached(Vec<PluginCompletions>),
    /// A new place: ask the plugins as ask number `n`.
    Ask(u64),
}

/// A completion request: the text and cursor it was made for, and whether it answered nothing.
struct Request {
    seq: u64,
    text: Rope,
    offset: usize,
    empty: bool,
}

impl IntelState {
    /// Records a completion request as the latest one. Gives back its sequence number.
    fn note_request(&mut self, text: Rope, offset: usize) -> u64 {
        let seq = self.request.as_ref().map_or(1, |r| r.seq + 1);
        self.request = Some(Request {
            seq,
            text,
            offset,
            empty: false,
        });
        seq
    }

    /// Decides what the popup at (`text`, `offset`) does about plugins. A new place starts a new
    /// ask and drops the answers of the old one.
    fn plan_plugins(&mut self, text: &Rope, offset: usize, has_backend: bool) -> PluginPlan {
        if !has_backend {
            return PluginPlan::Off;
        }
        if let Some(p) = self
            .plugin
            .as_ref()
            .filter(|p| p.offset == offset && p.text.len() == text.len() && p.text == *text)
        {
            return PluginPlan::Cached(p.answers.answers().to_vec());
        }
        self.next_ask += 1;
        let ask = self.next_ask;
        self.plugin = Some(PluginAsk {
            text: text.clone(),
            offset,
            answers: PluginAnswers::new(ask),
        });
        PluginPlan::Ask(ask)
    }

    /// Keeps a plugin's answer to ask `ask`, unless a newer ask replaced it. Whether it was kept.
    fn accept_plugin(&mut self, ask: u64, answer: PluginCompletions) -> bool {
        self.plugin
            .as_mut()
            .is_some_and(|p| p.answers.accept(ask, answer))
    }

    /// Notes that request `seq` was answered with no items, unless a newer one took over.
    fn note_empty(&mut self, seq: u64, empty: bool) {
        if let Some(r) = self.request.as_mut().filter(|r| r.seq == seq) {
            r.empty = empty;
        }
    }
}

/// Keeps the keys of a load marked asked only while the load can still finish.
///
/// Dropped before [`LoadGuard::done`] (the future was cancelled, or its task went away), it takes
/// the keys out of the asked set so a later pass requests them again. A load that ran to its end
/// stays asked, so a key that fails does not loop.
struct LoadGuard {
    handle: IntelHandle,
    keys: Vec<CacheKey>,
    complete: bool,
}

impl LoadGuard {
    fn done(&mut self) {
        self.complete = true;
    }
}

impl Drop for LoadGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.handle.borrow_mut().intel.unask(&self.keys);
        }
    }
}

/// Runs `loading` for `keys` that were just marked asked, with the guard on them.
///
/// The guard exists from this call on, not from the first poll: a future dropped unstarted gives
/// its keys back too.
fn guarded<F: std::future::Future<Output = ()>>(
    handle: IntelHandle,
    keys: Vec<CacheKey>,
    loading: F,
) -> impl std::future::Future<Output = ()> {
    let mut guard = LoadGuard {
        handle,
        keys,
        complete: false,
    };
    async move {
        loading.await;
        guard.done();
    }
}

/// Loads `keys` (just marked asked by `fresh`) in a task of its own that no edit and no replaced
/// diagnose pass cancels, then runs `then`. Every load goes through here: a key is asked only
/// while a task that survives edits owns its load, and [`LoadGuard`] gives it back if even that
/// task is dropped.
fn spawn_owned_load(
    handle: &IntelHandle,
    editor: &WeakEntity<EditorState>,
    catalog: &Catalog,
    keys: Vec<CacheKey>,
    window: &Window,
    cx: &App,
    then: impl FnOnce(&mut AsyncWindowContext) + 'static,
) {
    // Built here, not in the task: a task dropped before its first poll still gives the keys back.
    let owned = owned_load(handle, catalog, keys);
    let editor = editor.clone();
    window
        .spawn(cx, async move |cx| {
            owned.await;
            then(cx);
            // The load bumped the snapshot generation; `sync_intellisense` runs on a render, so
            // ask for one, or an unfocused editor keeps its stale underlines until the next input.
            editor.update(cx, |_, cx| cx.notify()).ok();
            cx.update(|window, _| window.refresh()).ok();
        })
        .detach();
}

/// The load of `keys` with its [`LoadGuard`] already on, so the guard exists from the call.
fn owned_load(
    handle: &IntelHandle,
    catalog: &Catalog,
    keys: Vec<CacheKey>,
) -> impl std::future::Future<Output = ()> + 'static {
    guarded(handle.clone(), keys.clone(), load(catalog, keys))
}

/// The shared state; it lives on the foreground thread only.
pub(super) type IntelHandle = Rc<RefCell<IntelState>>;

/// Completion and hover over the catalog of the editor's connection.
pub(super) struct SqlProvider {
    handle: IntelHandle,
    editor: WeakEntity<EditorState>,
}

impl SqlProvider {
    /// Installs the providers and the label highlighter on `state`.
    pub(super) fn install(state: &Entity<EditorState>, handle: &IntelHandle, cx: &mut App) {
        let provider = Rc::new(Self {
            handle: handle.clone(),
            editor: state.downgrade(),
        });
        state.update(cx, |s, _| {
            let lsp = s.lsp_mut();
            lsp.completion_provider = Some(provider.clone());
            lsp.hover_provider = Some(provider);
            lsp.set_completion_highlighter(Rc::new(|query, item| {
                crate::editor::intellisense::highlight_ranges(query, &item.label)
            }));
        });
    }

    fn context(&self) -> Option<Context> {
        self.handle.borrow().ctx.clone()
    }
}

impl CompletionProvider for SqlProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let Some(ctx) = self.context() else {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        };
        let rope = text.clone();
        let snapshot = ctx.catalog.snapshot();
        let dialect = ctx.dialect;
        let (seq, plan) = {
            let mut state = self.handle.borrow_mut();
            let seq = state.note_request(rope.clone(), offset);
            (
                seq,
                state.plan_plugins(&rope, offset, ctx.plugins.is_some()),
            )
        };
        let (cached, ask) = match plan {
            PluginPlan::Off => (Vec::new(), None),
            PluginPlan::Cached(answers) => (answers, None),
            PluginPlan::Ask(n) => (Vec::new(), Some(n)),
        };
        // The plugins are never awaited here: the built-in popup is computed alone, the futures
        // only come back to be spawned, and their answers reopen the popup when they land.
        let backend = ctx.plugins.clone().filter(|_| ask.is_some());
        let driver = ctx.driver.clone();
        // The built-in task never touches the backend: the popup does not wait for the grants
        // file read that `backend.complete` may do.
        let compute = cx.background_executor().spawn(async move {
            let source = rope.to_string();
            let schema = CatalogSchema::new(snapshot, dialect);
            let popup = complete_popup(&source, offset, dialect, &schema);
            let range = lsp_types::Range {
                start: rope.offset_to_position(popup.replace.start),
                end: rope.offset_to_position(popup.replace.end),
            };
            let rows = merge_plugin_items(&popup.items, &cached);
            let request = plugin_request(&source, offset, dialect, driver.as_deref());
            (popup, range, rows, request)
        });
        let handle = self.handle.clone();
        let editor = self.editor.clone();
        window.spawn(cx, async move |cx| {
            let (popup, range, rows, request) = compute.await;
            handle.borrow_mut().note_empty(seq, popup.items.is_empty());
            if let (Some(ask), Some(backend), Some(request)) = (ask, backend, request) {
                let owner = handle.clone();
                let weak = editor.clone();
                // Detached: this task does not await it, so the popup below is returned first.
                cx.update(|window, cx| {
                    spawn_plugin_asks(&owner, &weak, backend, request, ask, window, cx);
                })
                .ok();
            }
            let plugin_items = rows.into_iter().map(|row| CompletionItem {
                kind: Some(plugin_kind(row.kind)),
                detail: Some(row.detail),
                sort_text: Some(row.sort_text),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: row.insert_text,
                })),
                ..CompletionItem::new_simple(row.label, String::new())
            });
            let mut items: Vec<CompletionItem> = popup
                .items
                .into_iter()
                .map(|item| CompletionItem {
                    kind: Some(item_kind(item.kind)),
                    detail: item.detail,
                    sort_text: Some(item.sort_text),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range,
                        new_text: item.insert_text,
                    })),
                    ..CompletionItem::new_simple(item.label, String::new())
                })
                .collect();
            items.extend(plugin_items);
            let loads = handle.borrow_mut().intel.fresh(popup.loads);
            if !loads.is_empty() {
                // The reopen looks at the latest request, not this one: a key typed during the
                // load makes its own request, whose loads `fresh` swallowed, and it is that
                // popup that has to be asked for again.
                let owner = handle.clone();
                let weak = editor.clone();
                cx.update(|window, cx| {
                    spawn_owned_load(&owner, &weak, &ctx.catalog, loads, window, cx, move |cx| {
                        reopen(&editor, &handle, cx);
                    });
                })
                .ok();
            }
            Ok(CompletionResponse::Array(items))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        is_completion_trigger(new_text)
    }
}

/// Starts the plugin asks of one place in a task of its own. The backend call (which may read the
/// grants file) runs on the background executor, each answer in a task of its own; a landed answer
/// reopens the popup. Nothing here is awaited by the built-in popup.
fn spawn_plugin_asks(
    handle: &IntelHandle,
    editor: &WeakEntity<EditorState>,
    backend: Arc<dyn PluginsBackend>,
    request: sqler_plugin_api::CompletionRequest,
    ask: u64,
    window: &Window,
    cx: &App,
) {
    let calls = cx
        .background_executor()
        .spawn(async move { backend.complete(&request) });
    let handle = handle.clone();
    let editor = editor.clone();
    window
        .spawn(cx, async move |cx| {
            let asks = calls.await;
            cx.update(|window, cx| {
                for fut in asks {
                    let handle = handle.clone();
                    let editor = editor.clone();
                    window
                        .spawn(cx, async move |cx| {
                            // A failed plugin is skipped: its fault is counted by the host.
                            if let Ok(answer) = fut.await
                                && handle.borrow_mut().accept_plugin(ask, answer)
                            {
                                reopen(&editor, &handle, cx);
                            }
                        })
                        .detach();
                }
            })
            .ok();
        })
        .detach();
}

/// Whether the popup is asked for again once the loads are done: only when it still means the
/// same place. The text and the cursor did not move, the editor has the focus, and the menu either
/// shows the old answer or never showed one (AC2).
///
/// The text is compared, not a revision counter: the edit that triggered the request has its
/// `Change` event delivered after the provider is called, so a counter read at request time is
/// always stale by the time the loads end.
fn should_reopen(still: Still, focused: bool, open: bool, was_empty: bool) -> bool {
    still.text && still.cursor && focused && (open || was_empty)
}

/// What did not move since the request.
#[derive(Clone, Copy)]
struct Still {
    text: bool,
    cursor: bool,
}

/// Asks for the popup again once the loads are done, when [`should_reopen`] says it still fits.
fn reopen(editor: &WeakEntity<EditorState>, handle: &IntelHandle, cx: &mut AsyncWindowContext) {
    let (request, offset, was_empty) = {
        let state = handle.borrow();
        let Some(r) = state.request.as_ref() else {
            return;
        };
        (r.text.clone(), r.offset, r.empty)
    };
    let request = &request;
    editor
        .update_in(cx, |state, window, cx| {
            let still = Still {
                text: state.text().len() == request.len() && state.text() == request,
                cursor: state.cursor() == offset,
            };
            if !should_reopen(
                still,
                state.focus_handle(cx).is_focused(window),
                state.completion_menu_state().open,
                was_empty,
            ) {
                return;
            }
            state.dismiss_lsp_overlays(cx);
            state.show_completions(window, cx);
        })
        .ok();
}

fn plugin_kind(kind: CompletionKind) -> CompletionItemKind {
    match kind {
        CompletionKind::Keyword => CompletionItemKind::KEYWORD,
        CompletionKind::Table => CompletionItemKind::STRUCT,
        CompletionKind::Column => CompletionItemKind::FIELD,
        CompletionKind::Function => CompletionItemKind::FUNCTION,
        CompletionKind::Snippet => CompletionItemKind::SNIPPET,
        _ => CompletionItemKind::TEXT,
    }
}

fn item_kind(kind: ItemKind) -> CompletionItemKind {
    match kind {
        ItemKind::Column => CompletionItemKind::FIELD,
        ItemKind::Table | ItemKind::View | ItemKind::MaterializedView | ItemKind::Cte => {
            CompletionItemKind::STRUCT
        }
        ItemKind::Schema => CompletionItemKind::MODULE,
        ItemKind::Function => CompletionItemKind::FUNCTION,
        ItemKind::Keyword => CompletionItemKind::KEYWORD,
        ItemKind::Snippet => CompletionItemKind::SNIPPET,
        ItemKind::Alias => CompletionItemKind::VARIABLE,
        _ => CompletionItemKind::TEXT,
    }
}

impl HoverProvider for SqlProvider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let Some(ctx) = self.context() else {
            return Task::ready(Ok(None));
        };
        let rope = text.clone();
        let snapshot = ctx.catalog.snapshot();
        let dialect = ctx.dialect;
        let compute = cx.background_executor().spawn(async move {
            let source = rope.to_string();
            let schema = CatalogSchema::new(snapshot, dialect);
            hover_markdown(&source, offset, dialect, &schema).map(|(range, markdown)| Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: markdown,
                }),
                range: Some(lsp_types::Range {
                    start: rope.offset_to_position(range.start),
                    end: rope.offset_to_position(range.end),
                }),
            })
        });
        window.spawn(cx, async move |_| Ok(compute.await))
    }
}

/// One finding, with positions already worked out.
struct Finding {
    range: lsp_types::Range,
    severity: DiagnosticSeverity,
    code: &'static str,
    message: String,
}

/// Loads the bootstrap `keys` (just marked asked) in a task nothing cancels.
pub(super) fn preload(
    handle: &IntelHandle,
    editor: &WeakEntity<EditorState>,
    catalog: &Catalog,
    keys: Vec<CacheKey>,
    window: &Window,
    cx: &App,
) {
    spawn_owned_load(handle, editor, catalog, keys, window, cx, |_| {});
}

/// Diagnoses the text of `state` after `delay`, underlines the findings, and loads the columns of
/// the relations the text names. Loaded columns are diagnosed again.
///
/// The returned task replaces the editor's previous one, so a newer edit cancels an older pass.
pub(super) fn diagnose_task(
    state: &Entity<EditorState>,
    handle: &IntelHandle,
    delay: Duration,
    window: &Window,
    cx: &App,
) -> Task<()> {
    let editor = state.downgrade();
    let handle = handle.clone();
    window.spawn(cx, async move |cx| {
        if !delay.is_zero() {
            cx.background_executor().timer(delay).await;
        }
        // The loads this pass asks for run in tasks of their own: replacing this task (the next
        // edit) must not lose them. Their tables bump the snapshot generation, which makes
        // `sync_intellisense` diagnose again, so this task does not wait for them.
        if let Some(more) = diagnose_once(&editor, &handle, cx).await {
            let catalog = handle.borrow().ctx.as_ref().map(|c| c.catalog.clone());
            if let Some(catalog) = catalog {
                let owner = handle.clone();
                cx.update(|window, cx| {
                    spawn_owned_load(&owner, &editor, &catalog, more, window, cx, |_| {});
                })
                .ok();
            }
        }
    })
}

/// One diagnose pass. Gives back the keys to load before the next one, if any.
async fn diagnose_once(
    editor: &WeakEntity<EditorState>,
    handle: &IntelHandle,
    cx: &mut AsyncWindowContext,
) -> Option<Vec<CacheKey>> {
    let ctx = handle.borrow().ctx.clone();
    let revision = handle.borrow().intel.revision();
    let rope = editor.read_with(cx, |s, _| s.text().clone()).ok()?;
    let Some(ctx) = ctx else {
        apply(editor, handle, revision, &rope, Vec::new(), cx);
        return None;
    };
    let snapshot = ctx.catalog.snapshot();
    let dialect = ctx.dialect;
    let job_rope = rope.clone();
    let asked = handle.borrow().intel.asked();
    let (findings, keys) = cx
        .background_spawn(async move {
            let source = job_rope.to_string();
            let schema = CatalogSchema::new(snapshot, dialect);
            let findings = diagnose(&source, dialect, &schema)
                .into_iter()
                .map(|d| Finding {
                    range: lsp_types::Range {
                        start: job_rope.offset_to_position(d.range.start),
                        end: job_rope.offset_to_position(d.range.end),
                    },
                    severity: match d.severity {
                        Severity::Error => DiagnosticSeverity::ERROR,
                        Severity::Warning => DiagnosticSeverity::WARNING,
                        _ => DiagnosticSeverity::INFORMATION,
                    },
                    code: code_name(d.code),
                    message: d.message,
                })
                .collect::<Vec<_>>();
            let keys = prefetch_keys_skipping(&source, dialect, &schema, PREFETCH_LIMIT, &asked);
            (findings, keys)
        })
        .await;
    if !apply(editor, handle, revision, &rope, findings, cx) {
        return None;
    }
    let keys = handle.borrow_mut().intel.fresh(keys);
    (!keys.is_empty()).then_some(keys)
}

/// Puts `findings` on the editor, unless the text changed since `revision`. Whether it did.
fn apply(
    editor: &WeakEntity<EditorState>,
    handle: &IntelHandle,
    revision: u64,
    rope: &Rope,
    findings: Vec<Finding>,
    cx: &mut AsyncWindowContext,
) -> bool {
    if handle.borrow().intel.revision() != revision {
        return false;
    }
    editor
        .update(cx, |state, cx| {
            // `None` outside code-editor mode; underlines then do nothing.
            let Some(set) = state.diagnostics_mut() else {
                return false;
            };
            set.reset(rope);
            set.extend(findings.into_iter().map(|f| Diagnostic {
                range: f.range,
                severity: Some(f.severity),
                code: Some(NumberOrString::String(f.code.to_owned())),
                source: Some("sqler".to_owned()),
                message: f.message,
                ..Diagnostic::default()
            }));
            cx.notify();
            true
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::future::{Future, pending, ready};
    use std::pin::pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    use gpui_component::input::Rope;
    use sqler_catalog::{CacheKey, NodeKey};
    use sqler_driver_api::sqler_core::{ObjectKind, ObjectPath};

    use super::{
        IntelHandle, IntelState, PluginCompletions, PluginPlan, Still, guarded, owned_load,
        should_reopen,
    };
    use crate::editor::intellisense::synthetic::synthetic_catalog_with;

    fn keys() -> Vec<CacheKey> {
        [ObjectKind::Table, ObjectKind::View]
            .into_iter()
            .map(|k| CacheKey::Children(NodeKey::new(ObjectPath::root(), k)))
            .collect()
    }

    fn handle() -> IntelHandle {
        let h: IntelHandle = Rc::new(RefCell::new(IntelState::default()));
        h.borrow_mut().intel.bind(Some(1));
        h
    }

    /// Polls `fut` once, the way an executor starts a task.
    fn poll_once<F: Future<Output = ()>>(fut: F) -> bool {
        let mut cx = Context::from_waker(Waker::noop());
        matches!(pin!(fut).poll(&mut cx), Poll::Ready(()))
    }

    /// A load cancelled midway (the task dropped) gives its keys back; a later pass asks again.
    #[test]
    fn a_cancelled_load_is_requested_again() {
        let h = handle();
        let asked = h.borrow_mut().intel.fresh(keys());
        assert_eq!(asked, keys());
        // Started, never finished, then dropped: what an edit does to a diagnose task.
        let mut fut = Box::pin(guarded(h.clone(), asked, pending()));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        // Still in flight: nobody asks twice.
        assert_eq!(h.borrow_mut().intel.fresh(keys()).len(), 0);
        drop(fut);
        assert_eq!(h.borrow_mut().intel.fresh(keys()), keys());
    }

    /// A load that was never polled and is dropped gives its keys back as well.
    #[test]
    fn an_unstarted_load_is_requested_again() {
        let h = handle();
        let asked = h.borrow_mut().intel.fresh(keys());
        drop(guarded(h.clone(), asked, ready(())));
        assert_eq!(h.borrow_mut().intel.fresh(keys()), keys());
    }

    /// The construction `spawn_owned_load` uses: dropped unstarted, the load gives its keys back.
    #[test]
    fn an_unpolled_owned_load_is_requested_again() {
        let h = handle();
        let (catalog, _worker, _) = synthetic_catalog_with(3, 2);
        let asked = h.borrow_mut().intel.fresh(keys());
        let owned = owned_load(&h, &catalog, asked);
        assert_eq!(h.borrow_mut().intel.fresh(keys()).len(), 0);
        drop(owned);
        assert_eq!(h.borrow_mut().intel.fresh(keys()), keys());
    }

    /// No loop: a load that ran to its end keeps its keys asked, so they are not requested again.
    #[test]
    fn a_completed_load_is_not_requested_again() {
        let h = handle();
        let asked = h.borrow_mut().intel.fresh(keys());
        assert!(poll_once(guarded(h.clone(), asked, ready(()))));
        assert_eq!(h.borrow_mut().intel.fresh(keys()).len(), 0);
    }

    /// The reopen compares against the latest request: a key typed during the load replaces it.
    #[test]
    fn the_latest_request_is_the_one_kept() {
        let mut s = IntelState::default();
        let first = s.note_request(Rope::from("SELECT u. FROM users u"), 10);
        let second = s.note_request(Rope::from("SELECT u.i FROM users u"), 11);
        assert!(second > first);
        s.note_empty(first, true);
        let r = s.request.as_ref().expect("a request");
        assert_eq!((r.seq, r.offset, r.empty), (second, 11, false));
        assert_eq!(r.text, Rope::from("SELECT u.i FROM users u"));
        s.note_empty(second, true);
        assert!(s.request.as_ref().is_some_and(|r| r.empty));
    }

    fn answer(items: &[&str]) -> PluginCompletions {
        PluginCompletions {
            plugin: "alpha".parse().expect("id"),
            source: "Alpha".to_owned(),
            items: items
                .iter()
                .map(|l| sqler_plugin_api::CompletionItem {
                    label: (*l).to_owned(),
                    kind: sqler_plugin_api::CompletionKind::Table,
                    insert_text: (*l).to_owned(),
                    detail: None,
                })
                .collect(),
        }
    }

    /// Without a backend nobody is asked.
    #[test]
    fn no_backend_means_no_plugin_ask() {
        let mut s = IntelState::default();
        assert!(matches!(
            s.plan_plugins(&Rope::from("SELECT "), 7, false),
            PluginPlan::Off
        ));
    }

    /// The same text and cursor read the cache and ask no more plugins.
    #[test]
    fn a_cache_hit_asks_no_more_plugins() {
        let mut s = IntelState::default();
        let text = Rope::from("SELECT * FROM u");
        let PluginPlan::Ask(ask) = s.plan_plugins(&text, 15, true) else {
            panic!("the first pass asks");
        };
        assert!(s.accept_plugin(ask, answer(&["users"])));
        let PluginPlan::Cached(got) = s.plan_plugins(&text, 15, true) else {
            panic!("the same place is cached");
        };
        assert_eq!(got, vec![answer(&["users"])]);
    }

    /// A moved cursor or changed text starts a new ask and drops the old answers; a late answer
    /// of the old ask is refused.
    #[test]
    fn a_new_place_asks_again_and_drops_the_old_answers() {
        let mut s = IntelState::default();
        let text = Rope::from("SELECT * FROM u");
        let PluginPlan::Ask(first) = s.plan_plugins(&text, 15, true) else {
            panic!("ask");
        };
        assert!(s.accept_plugin(first, answer(&["users"])));
        let PluginPlan::Ask(second) = s.plan_plugins(&text, 14, true) else {
            panic!("a moved cursor asks");
        };
        assert!(second > first);
        assert!(!s.accept_plugin(first, answer(&["late"])));
        assert!(matches!(
            s.plan_plugins(&text, 14, true),
            PluginPlan::Cached(a) if a.is_empty()
        ));
        let edited = Rope::from("SELECT * FROM us");
        assert!(matches!(
            s.plan_plugins(&edited, 14, true),
            PluginPlan::Ask(_)
        ));
    }

    /// The built-in popup does not wait for the plugins: the first answer at a place has no plugin
    /// row even though the scripted answer is ready at once. The backend (and its grants read) is
    /// called once, off the built-in task; the arrived answer shows in the next popup at the same
    /// place, which asks nobody again.
    #[gpui::test]
    fn the_builtin_popup_comes_first_and_the_plugin_rows_follow(cx: &mut gpui::TestAppContext) {
        use std::sync::Arc;

        use gpui_component::input::{CompletionProvider as _, EditorState};
        use lsp_types::{CompletionContext, CompletionResponse, CompletionTriggerKind};

        use super::{Context as Ctx, SqlProvider};
        use crate::plugins::PluginsBackend;
        use crate::plugins::backend::FakeBackend;

        cx.update(gpui_component::init);
        let (state, cx) = cx.add_window_view(EditorState::new);
        let (catalog, _worker, _handle) = synthetic_catalog_with(2, 2);
        let backend = FakeBackend::new(crate::plugins::PluginSnapshot::default());
        backend
            .completions
            .lock()
            .expect("lock")
            .push(answer(&["zz_plugin_row"]));
        let shared = handle();
        shared.borrow_mut().ctx = Some(Ctx {
            catalog,
            dialect: sqler_sql::Dialect::Postgres,
            plugins: Some(Arc::clone(&backend) as Arc<dyn PluginsBackend>),
            driver: Some("sqlite".to_owned()),
        });
        let provider = SqlProvider {
            handle: shared,
            editor: state.downgrade(),
        };
        let text = Rope::from("SELECT * FROM ");
        let labels = |response: CompletionResponse| match response {
            CompletionResponse::Array(items) => {
                items.into_iter().map(|i| i.label).collect::<Vec<_>>()
            }
            CompletionResponse::List(l) => l.items.into_iter().map(|i| i.label).collect(),
        };
        let ask = |cx: &mut gpui::VisualTestContext| {
            let task = cx.update(|window, cx| {
                provider.completions(
                    &text,
                    text.len(),
                    CompletionContext {
                        trigger_kind: CompletionTriggerKind::INVOKED,
                        trigger_character: None,
                    },
                    window,
                    cx,
                )
            });
            let got = cx.foreground_executor().block_test(task);
            labels(got.expect("a popup"))
        };
        let first = ask(cx);
        assert!(!first.iter().any(|l| l == "zz_plugin_row"));
        cx.run_until_parked();
        assert_eq!(backend.complete_requests.lock().expect("lock").len(), 1);
        let second = ask(cx);
        cx.run_until_parked();
        assert_eq!(
            second.iter().filter(|l| *l == "zz_plugin_row").count(),
            1,
            "the arrived answer is merged"
        );
        assert_eq!(second.len(), first.len() + 1, "after the built-in rows");
        assert_eq!(second.last().map(String::as_str), Some("zz_plugin_row"));
        assert_eq!(
            backend.complete_requests.lock().expect("lock").len(),
            1,
            "the same place asks no plugin again"
        );
    }

    const STILL: Still = Still {
        text: true,
        cursor: true,
    };

    /// The order of events for a typed `u.`: the request is made, the loads end, the popup is
    /// asked again. The editor text equals the request text then, whatever the revision did.
    #[test]
    fn typing_reopens_once_the_columns_loaded() {
        assert!(should_reopen(STILL, true, false, true));
        assert!(should_reopen(STILL, true, true, false));
    }

    #[test]
    fn a_moved_or_changed_editor_does_not_reopen() {
        assert!(!should_reopen(
            Still {
                text: false,
                ..STILL
            },
            true,
            true,
            true
        ));
        assert!(!should_reopen(
            Still {
                cursor: false,
                ..STILL
            },
            true,
            true,
            true
        ));
        assert!(!should_reopen(STILL, false, true, true));
    }

    #[test]
    fn a_closed_menu_that_had_items_stays_closed() {
        assert!(!should_reopen(STILL, true, false, false));
    }
}
