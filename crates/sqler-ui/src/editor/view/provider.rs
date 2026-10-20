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

use crate::editor::intellisense::{
    CatalogSchema, PREFETCH_LIMIT, TabIntel, code_name, complete_popup, hover_markdown,
    is_completion_trigger, load, prefetch_keys,
};

/// What the providers of one editor read: the connection's catalog and dialect.
#[derive(Clone)]
pub(super) struct Context {
    pub(super) catalog: Catalog,
    pub(super) dialect: Dialect,
}

/// The state shared by the providers, the diagnose task and the view of one editor.
#[derive(Default)]
pub(super) struct IntelState {
    pub(super) intel: TabIntel,
    pub(super) ctx: Option<Context>,
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
        let request_rope = rope.clone();
        let compute = cx.background_executor().spawn(async move {
            let source = rope.to_string();
            let schema = CatalogSchema::new(snapshot, dialect);
            let popup = complete_popup(&source, offset, dialect, &schema);
            let range = lsp_types::Range {
                start: rope.offset_to_position(popup.replace.start),
                end: rope.offset_to_position(popup.replace.end),
            };
            (popup, range)
        });
        let handle = self.handle.clone();
        let editor = self.editor.clone();
        window.spawn(cx, async move |cx| {
            let (popup, range) = compute.await;
            let empty = popup.items.is_empty();
            let items: Vec<CompletionItem> = popup
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
            let loads = handle.borrow_mut().intel.fresh(popup.loads);
            if !loads.is_empty() {
                let ask_again = cx.update(|window, cx| {
                    window.spawn(cx, async move |cx| {
                        load(&ctx.catalog, loads).await;
                        reopen(&editor, &request_rope, offset, empty, cx);
                    })
                });
                if let Ok(task) = ask_again {
                    task.detach();
                }
            }
            Ok(CompletionResponse::Array(items))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        is_completion_trigger(new_text)
    }
}

/// Whether the popup is asked for again once the loads are done: only when it still means the
/// same place. The text and the cursor did not move, the editor has the focus, and the menu either
/// shows the old answer or never showed one (AC2).
///
/// The text is compared, not a revision counter: the edit that triggered the request has its
/// `Change` event delivered after the provider is called, so a counter read at request time is
/// always stale by the time the loads end.
fn should_reopen(
    same_text: bool,
    same_cursor: bool,
    focused: bool,
    open: bool,
    was_empty: bool,
) -> bool {
    same_text && same_cursor && focused && (open || was_empty)
}

/// Asks for the popup again once the loads are done, when [`should_reopen`] says it still fits.
fn reopen(
    editor: &WeakEntity<EditorState>,
    request: &Rope,
    offset: usize,
    was_empty: bool,
    cx: &mut AsyncWindowContext,
) {
    editor
        .update_in(cx, |state, window, cx| {
            let same_text = state.text().len() == request.len()
                && state.text().to_string() == request.to_string();
            if !should_reopen(
                same_text,
                state.cursor() == offset,
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

/// Loads `keys` in a task of its own that nothing cancels.
pub(super) fn preload(catalog: &Catalog, keys: Vec<CacheKey>, window: &Window, cx: &App) {
    let loading = load(catalog, keys);
    window.spawn(cx, async move |_| loading.await).detach();
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
        // Each round that loads something runs once more; `fresh` stops a key from being asked
        // twice, so the rounds end.
        while let Some(more) = diagnose_once(&editor, &handle, cx).await {
            let catalog = handle.borrow().ctx.as_ref().map(|c| c.catalog.clone());
            let Some(catalog) = catalog else { break };
            load(&catalog, more).await;
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
            let keys = prefetch_keys(&source, dialect, &schema, PREFETCH_LIMIT);
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
    use super::should_reopen;

    /// The order of events for a typed `u.`: the request is made, the loads end, the popup is
    /// asked again. The editor text equals the request text then, whatever the revision did.
    #[test]
    fn typing_reopens_once_the_columns_loaded() {
        assert!(should_reopen(true, true, true, false, true));
        assert!(should_reopen(true, true, true, true, false));
    }

    #[test]
    fn a_moved_or_changed_editor_does_not_reopen() {
        assert!(!should_reopen(false, true, true, true, true));
        assert!(!should_reopen(true, false, true, true, true));
        assert!(!should_reopen(true, true, false, true, true));
    }

    #[test]
    fn a_closed_menu_that_had_items_stays_closed() {
        assert!(!should_reopen(true, true, true, false, false));
    }
}
