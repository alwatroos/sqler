# SQL editor

The SQL editor is a tab bound to one connection. Its header shows the connection name, colour and environment (a `prod` connection stands out), like every other tab. The editor has its own session, so a long query in one editor does not block another tab.

## Open an editor

- Run `editor:new` ("Editor: New SQL Editor", Cmd+N on macOS, Ctrl+N elsewhere). It opens an editor for the connection of the active tab, or for the connection selected in the sidebar. With neither, SQLer shows "Select a connection in the sidebar first."
- In the [object browser](object-browser.md), choose "Open in SQL Editor" (`object:open-in-editor`) on a table or view. The editor opens with a `SELECT *` for that object. The command `object:open-tab-in-editor` does the same for the DDL tab that is active, and the DDL tab has an "Open in Editor" button next to Copy.
- `dev:open-large-sql-script` ("Open Synthetic 10,000-Line SQL Script") opens an editor filled with a generated script of 10,000 lines. Use it to check that scrolling and typing stay smooth.

## Highlighting

SQL is highlighted with tree-sitter. The colours follow the theme, so they change with light, dark or system mode (see [Settings](settings.md)). Line numbers are shown.

## Multi-cursor editing

- Alt-click adds a cursor.
- `editor:add-cursor-above` and `editor:add-cursor-below` add a cursor on the line above or below. Native chords are listed in [Keybindings](keybindings.md#sql-editor).
- `editor:add-next-occurrence` (Cmd+D on macOS, Ctrl+D elsewhere): with no selection it selects the word under the cursor; pressed again it adds the next occurrence of the selected text, and wraps around to the start. It stops when the next match is already selected. Words are letters, digits, `_` and `$`.

Format and add-next-occurrence read the active selection only; with several cursors the other selections are left as they are.

## Bracket matching

When the cursor is next to `(`, `)`, `[`, `]`, `{` or `}`, both brackets of the pair are framed. Brackets inside strings, quoted identifiers and comments are ignored, with the rules of the connection's dialect (for example `$$...$$` bodies on `PostgreSQL`, backticks and `#` comments on `MySQL`). A bracket without a partner is not framed. `editor:go-to-matching-bracket` selects the partner bracket (and shows a message when the cursor is not next to a bracket with a partner).

## Format

`editor:format-sql` (Shift+Alt+F) formats the selection, or the whole text when nothing is selected, with the formatter for the connection's dialect. It is one undo step. Nothing happens, and no undo step is recorded, when the text is already formatted. If the text has an unterminated string, quoted identifier or comment, SQLer shows where (line and column) and leaves the text unchanged. Formatting is not available for connections whose driver has no SQL dialect.

## Running queries

The editor hands its text, selection and cursor to the query run commands, and results appear in the results area of the query tab. Until the query runner is wired in, the editor opens, edits and formats but does not execute.

## Limits

- Format and add-next-occurrence use the active selection only.
- Completion, hover and diagnostics, history and restoring editors after a restart are not part of this editor yet.

Design: [ADR-0025](../adr/0025-sql-editor-component.md) and [UI workspace](../architecture/ui.md#sql-editor).
