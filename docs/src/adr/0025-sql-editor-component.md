# 0025 SQL editor component: gpui-component editor with headless logic

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

The SQL editor (T9) needs syntax highlighting for light and dark themes, multi-cursor editing, bracket matching, format as one undo step, and a way to hand the buffer to the query run commands (T8). It must stay responsive on a 10,000-line script. AGENTS.md §12 forbids business logic in views, and views are build-only on CI, so every rule that can be tested must live outside the GPUI code. AGENTS.md §8 requires an ADR for a new public API (`sqler_ui::editor`).

## Decision drivers

- Highlighting through tree-sitter SQL, already enabled in the workspace `Cargo.toml`.
- No new dependency and no second text widget to maintain.
- Editor logic testable without a window.
- A narrow seam to T8, which is not merged yet.

## Considered options

1. `EditorState` of `gpui-component` 0.8.0 in code-editor mode (language `sql`), plus headless logic for what it lacks.
2. Our own editor widget on GPUI primitives.

## Decision outcome

Chosen: option 1.

An own widget needs text layout, IME, selection, undo, scrolling, line numbers and a highlighter. That is the cost of a separate project, and none of it is specific to SQLer. The component already provides highlighting through tree-sitter, line numbers, undo and redo, IME and multi-cursor.

**Where the tab lives.** The existing `TabKind::Query` tab (one per connection, with its own session) is the SQL editor tab. No new variant. The header shows the connection name, colour and environment badge as for every tab. The text lives in an `EditorState`; the highlight theme follows `Theme::change` (default light and dark highlight themes of `gpui-component`).

**Verified gaps in `gpui-component` 0.8.0 and how they are closed.**

| Gap | How it is closed |
| --- | ---------------- |
| No "add next occurrence" (Cmd+D style). Multi-cursor itself exists: Alt-click, `AddCursorAbove`, `AddCursorBelow`, `add_selection`. | `editor::occurrence` finds the word under the cursor (identifier characters: alphanumeric, `_`, `$`) and the next match, wrapping around; the view adds it with `add_selection`. The chain of added matches is kept per tab and stops when a match wraps onto an already added range. |
| No bracket matching. | `editor::brackets` builds a sorted index of `()`, `[]`, `{}` pairs that skips strings, quoted identifiers and comments (per dialect: dollar quotes and nested block comments for `PostgreSQL`; backticks and `#` comments for `MySQL`/`MariaDB`). The view frames both brackets with a `RangeDecorationCollection`. The index is rebuilt only when the text changed and the cursor touches a bracket. |
| No SQL formatter. | `editor::format` calls `sqler_sql::format` (ADR-0020) for the connection's dialect and returns one `TextEdit`; the view applies it with `EditorState::replace`, which records a single undo step. |

**Headless core.** All logic works on an `EditorBuffer` (text, selection, cursor; byte offsets on char boundaries) that the view copies out of `EditorState` when a command runs. The logic returns an `Effect` (`EditorEdit`, `EditorSelect`, `EditorAddSelection`, `EditorKey`) which the view applies, as for every other workspace action. Every action is a command with an ID (`editor:new`, `editor:format-sql`, `editor:add-next-occurrence`, `editor:add-cursor-above`, `editor:add-cursor-below`, `editor:go-to-matching-bracket`, plus `dev:open-large-sql-script`).

**Active selection only.** `gpui-component` exposes only the active selection. With several cursors, format and add-next-occurrence read the active one. This is a documented limit, not a bug.

**Seam to query execution.** The editor does not run SQL. It calls a `RunPort` trait (`run(workspace, tab, RunMode, EditorBuffer)` and `cancel`), where `RunMode` is `Selection`, `Statement` or `All`. Tests use a fake port. The adapter over the T8 run commands is added once T8 is merged; until then the editor opens, edits and formats but does not execute. Nothing under `query/**` is touched.

**Performance.** `cargo bench -p sqler-ui --bench editor_buffer` measures bracket index build and lookup, format and next-occurrence on a 10,000-line synthetic script (`dev:open-large-sql-script` opens the same script in a tab). Budget: build and format p95 below 16 ms. Scrolling and typing speed of the widget itself is a maintainer check on a display.

## Consequences

- Positive: no new dependency, no widget to maintain, highlighting and undo come with the component, and the rules we add are unit-tested.
- Positive: the `RunPort` seam lets T8 and T9 merge in either order.
- Negative: we depend on the behaviour of `gpui-component` 0.8.0 internals such as `add_selection`; a bump of `gpui-component` must re-check the gaps above, and some may disappear (then the headless code is removed).
- Negative: with several cursors only the active selection is formatted or extended.
- Negative: the bracket index is O(n) per rebuild; it is rebuilt only when needed and benchmarked.
- Rejected: own editor widget, because of cost.
