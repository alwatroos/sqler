# SQL editor

The SQL editor is a tab bound to one connection. Its header shows the connection name, colour and environment (a `prod` connection stands out), like every other tab. The editor has its own session, so a long query in one editor does not block another tab. The editor fills the tab below its toolbar (and the AI prompt bar when it is shown) and follows every resize of the window, the side panel, the bottom panel and editor splits.

## Open an editor

- Run `editor:new` ("Editor: New SQL Editor", Cmd+N on macOS, Ctrl+N elsewhere). It opens an editor for the connection of the active tab, or for the connection selected in the sidebar. With neither, SQLer shows "Select a connection in the sidebar first."
- In the [object browser](object-browser.md), choose "Open in SQL Editor" (`object:open-in-editor`) on a table or view. The editor opens with a `SELECT *` for that object. The command `object:open-tab-in-editor` does the same for the DDL tab that is active. Scripts generated from the DDL tab open directly in an editor tab.
- `dev:open-large-sql-script` ("Open Synthetic 10,000-Line SQL Script") opens an editor filled with a generated script of 10,000 lines. Use it to check that scrolling and typing stay smooth.

## Highlighting

SQL is highlighted with tree-sitter. The colours follow the theme, so they change with light, dark or system mode (see [Settings](settings.md)). Line numbers are shown.

## Multi-cursor editing

- Alt-click adds a cursor.
- `editor:add-cursor-above` and `editor:add-cursor-below` add a cursor on the line above or below. Native chords are listed in [Keybindings](keybindings.md#sql-editor).
- `editor:add-next-occurrence` (Cmd+D on macOS, Ctrl+D elsewhere): with no selection it selects the word under the cursor; pressed again it adds the next occurrence of the selected text, and wraps around to the start. It stops when the next match is already selected. Words are letters, digits, `_` and `$`. When the first selection came from the word under the cursor, only whole words match (`id` does not match inside `valid`); with a selection you made yourself, any occurrence matches.

Format and add-next-occurrence read the active selection only; with several cursors the other selections are left as they are.

## Bracket matching

When the cursor is next to `(`, `)`, `[`, `]`, `{` or `}`, both brackets of the pair are framed. Brackets inside strings, quoted identifiers and comments are ignored, with the rules of the connection's dialect (for example `$$...$$` bodies on `PostgreSQL`, backticks and `#` comments on `MySQL`). A bracket without a partner is not framed. `editor:go-to-matching-bracket` selects the partner bracket (and shows a message when the cursor is not next to a bracket with a partner).

## Format

`editor:format-sql` (Shift+Alt+F) formats the selection, or the whole text when nothing is selected, with the formatter for the connection's dialect. It is one undo step. Nothing happens, and no undo step is recorded, when the text is already formatted. If the text has an unterminated string, quoted identifier or comment, SQLer shows where (line and column) and leaves the text unchanged. Formatting is not available for connections whose driver has no SQL dialect.

## Comments

`editor:toggle-line-comment` (Cmd+/ on macOS, Ctrl+/ on Windows and Linux) toggles a `-- ` comment on the cursor's line, or on every line of the selection. When every non-blank line is already commented, the comments are removed; otherwise all non-blank lines are commented at the smallest indentation, so the indentation is kept. Blank lines are left alone, and the selection stays on the same text. It is one undo step.

`editor:toggle-block-comment` (Shift+Alt+A) wraps the selection in `/* */`, or unwraps it when the selection is already a block comment. With nothing selected it inserts `/*  */` and puts the cursor inside.

## Running queries

The editor hands its text, selection and cursor to the query run commands, and the results appear in the Results view of the bottom panel, one grid per statement.

| Command | macOS | Windows and Linux | Runs |
| --- | --- | --- | --- |
| `query:run-selection` | Cmd+E | Ctrl+E | the selected text (the statement at the cursor when nothing is selected) |
| `query:run-statement` | Cmd+Enter | Ctrl+Enter | the selection when there is one, else the statement at the cursor |
| `query:run-all` | Cmd+Shift+Enter | Ctrl+Shift+Enter | every statement, in order |
| `query:cancel` | Cmd+. | Ctrl+. | cancels the running statement |

With several cursors, the run commands read the main selection and cursor. Placeholders, the production confirmation and cancelling are described in [Running queries](running-queries.md).

## Intellisense

Completion, hover and diagnostics need a connection whose object browser is open and loaded; until then the editor stays silent.

- **Completion.** A popup opens while you type an identifier character or `.`. It offers keywords, functions, snippets, schemas, tables, views and columns, with the columns of the tables in the statement first. Press `.` after an alias (`SELECT u.` with `FROM users u`) to list that table's columns. `editor:trigger-completion` (Ctrl+Space, also in the palette under `Editor:`) opens the popup on demand.
- **Plugin completion.** When a plugin with a completion provider is installed, approved and switched on, its rows come after the built-in ones, marked with the plugin's name in the detail column. The popup never waits for a plugin: the plugin rows appear a moment later if the cursor has not moved, and a slow plugin (more than 250 ms) is skipped. See [Completion providers](../plugin-development/completion.md).
- **Hover.** Rest the mouse on a column to see its type, whether it is `NOT NULL` and the target of its foreign key; on a table or view to see its kind and schema.
- **Diagnostics.** About 300 ms after you stop typing, unknown schemas, tables, columns and qualifiers are underlined as errors and ambiguous columns as warnings; hover the underline for the message. Names whose columns or lists are not loaded are never flagged.
- **On-demand loading.** Columns of a table are loaded in the background when you reference it. The popup and the underlines update when they arrive. A catalog refresh in the object browser also updates suggestions without reopening the tab.

Ctrl+Space is the default chord on every platform. On macOS it switches the input source by default (System Settings, Keyboard, Keyboard Shortcuts, Input Sources), so the system may take the key before the editor sees it. Either turn that system shortcut off or rebind `editor:trigger-completion` in `keybindings.toml` (see [Keybindings](keybindings.md)); the command is always available from the command palette.

## Limits

- Format and add-next-occurrence use the active selection only.
- Snippets are inserted as plain text, without tab stops.
- A load that failed is not retried until the connection's browser reconnects.
- History and restoring editors after a restart are not part of this editor yet.

Design: [ADR-0025](../adr/0025-sql-editor-component.md), [ADR-0026](../adr/0026-editor-intellisense.md) and [UI workspace](../architecture/ui.md#sql-editor).
