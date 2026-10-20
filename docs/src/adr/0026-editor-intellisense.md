# 0026 Editor intellisense: completion, hover and diagnostics in the SQL editor

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

The SQL editor (ADR-0025) has no completion, hover or diagnostics. The pieces exist: `sqler-completion` answers `complete`, `hover` and `diagnose` over a `SchemaSource` (ADR-0023), and the catalog gives immutable snapshots and loads columns on demand (ADR-0022). T10 joins them in the editor without stalling the UI on a 5,000-table schema or a 10,000-line script. AGENTS.md §8 requires an ADR for a new public API (`sqler_ui::editor::intellisense`) and a new direct dependency (`lsp-types`).

## Decision drivers

- Use the hooks `gpui-component` 0.8.0 already has: `EditorState::lsp_mut()` with `CompletionProvider` and `HoverProvider`, `diagnostics_mut()` and the `ShowCompletions` action.
- Business logic headless and tested; views build-only (AGENTS.md §12).
- No work on the UI thread proportional to the script or the schema.
- A catalog refresh must show up without reopening the tab.

## Considered options

1. A GPUI-free adapter module plus a thin GUI provider; work on GPUI's background executor, one fresh snapshot per request.
2. The same, but computed synchronously on the UI thread.
3. Our own popup and underline rendering.

## Decision outcome

Chosen: option 1.

**Adapter.** `editor::intellisense` holds `CatalogSchema` (a `SchemaSource` over a `CatalogSnapshot`), the mapping from `SchemaRequest` to `CacheKey` (`keys_for`, `bootstrap_keys`, `prefetch_keys`), `load`, the conversions to popup, hover and diagnostic data (`complete_popup`, `hover_markdown`, `code_name`), and `TabIntel`, the per-tab record of asked keys and generations. It does not depend on GPUI. The GUI provider (`editor/view/provider.rs`) only converts this data to `lsp-types` and `gpui-component` types.

**Catalog per tab.** The catalog is the connection's browser catalog (`Workspace::editor_catalog(tab)`, `Some` only when the browser is `Ready`). No second session and no `sqler-app` change. When it is `None` or the dialect is unknown, the providers answer empty.

**Threading.** Each request takes a fresh snapshot (ADR-0022) and computes on the background executor, so the next keystroke sees a refresh. Missing listings and columns load through the catalog's channel futures, awaited on the GPUI executor like `WorkspaceView::load_catalog`; the catalog worker already runs on tokio, so nothing calls `tokio::spawn` on the UI thread. Rejected: computing on the UI thread, because diagnose of a 10,000-line script plus the `Rope` to `String` copy would stall a frame.

**Diagnostics** are debounced by 300 ms, re-run on every new snapshot generation and after prefetch loads (columns of relations named in the text, at most 64 per pass). Failed loads are not retried until the browser reconnects.

**Command and chord.** `editor:trigger-completion` ("Editor: Trigger Completion") maps to `input::ShowCompletions`. Its default chord is Ctrl+Space on every platform. On macOS that chord switches the input source by default, so the system may intercept it; this is documented, the command is in the palette, and the user can rebind it.

**Snippets** are flattened to plain text (`${1:x}` becomes `x`, `$0` is removed), because the editor has no snippet engine.

**Dependency.** `lsp-types` 0.97 is added as a direct dependency (optional, in feature `gui`). It must be the same 0.97 line as the one `gpui-component` 0.8.0 uses, otherwise the provider traits see a different type.

**Reliance on internals.** After a load completes the popup is requested again if the menu is open or the earlier answer was empty. Whether the menu is open is read from `completion_menu_state()`, which is `#[doc(hidden)]` in `gpui-component`. A bump must re-check it; if it disappears, re-request only when the earlier answer was empty.

## Consequences

- Positive: logic is tested without a window; the UI thread does only conversions; refreshes and loads are picked up on the next keystroke or the next diagnose.
- Positive: the latency is measured by `cargo bench -p sqler-ui --bench editor_intellisense` (budget p95 16 ms, headless).
- Negative: a new public API and a direct `lsp-types` dependency whose version is coupled to `gpui-component`; a bump of `gpui-component` must re-check the version, `completion_menu_state` and `diagnostics_mut` (only in code-editor mode).
- Negative: each keystroke allocates a lazy slot vector per touched listing (about 40 KB for 5,000 entries); the bench keeps this under the budget.
- Negative: the first column list of a table appears one request after it is loaded; names that are not loaded are never flagged.
