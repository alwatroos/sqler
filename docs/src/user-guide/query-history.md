# Query history and saved queries

SQLer records every statement you run, and you can keep named queries. Both are available from the command palette and the sidebar.

## What is recorded

Each statement that finishes, fails or is cancelled in a SQL editor is one history entry: the SQL text, the connection, the start time, the duration, the outcome (`OK`, `Error` or `Cancelled`) and the row count when it is known. A script with three statements makes three entries.

- The SQL is stored as written, before placeholders are bound. Values you type into the parameter dialog are never stored.
- Error messages are not stored, only the outcome.
- Statements that look like they hold credentials (passwords, tokens, keys and similar) are stored with their string literals and comments replaced by `'<redacted>'`. This is a heuristic, not a guarantee: ordinary literals, for example an e-mail address in a `WHERE` clause, are kept. See [Settings](settings.md) and [ADR-0021](../adr/0021-query-history-and-saved-queries.md) for the rules.
- The newest 10,000 entries are kept. SQL longer than 256 KiB is cut.

The history lives in `history.jsonl` in the SQLer config directory.

## The history tab

Run `history:open` ("History: Open Query History"). It opens a History tab for the connection of the active tab, or for the connection selected in the sidebar. With neither, SQLer shows "Select a connection in the sidebar first." If a History tab for that connection is already open, it is activated instead of opening a second one.

The tab lists the newest 500 matching entries, newest first. Each row shows when it ran (`just now`, `5 min ago`, `3 h ago`, `2 d ago`), the connection, the outcome, the duration (`12 ms`, `1.2 s`) with the row count, and the first line of the SQL (cut at 200 characters).

- **Search**: type in the search box. Every word you type must appear in the SQL, in any order, ignoring case. For example `users where` finds entries that contain both words.
- **All connections**: tick the checkbox, or run `history:toggle-all-connections`, to search the history of every connection. By default only the tab's connection is shown.
- Click a row to select it, double-click to open it.

New entries appear in an open History tab after the next search refresh; the tab searches again whenever an entry is recorded.

## Open or run again

- **Open** (`history:open-entry`, "History: Open Entry in Editor") opens the selected entry in a new SQL editor tab for its connection. Nothing runs. If SQLer hid sensitive values in the entry (they show as `'<redacted>'`), a notice says you must enter them again before running it.
- **Run again** (`history:run-entry`, "History: Run Entry Again") opens a new editor tab, as above, and runs the whole entry once the connection is up. Every editor has its own session, so this always opens a new tab. If SQLer first asks you to trust an SSH host key, the editor only opens and nothing runs.

**Run again is refused for an entry with redacted values**: it shows "Run again is not available: sensitive values were redacted from this entry." and runs nothing, because the placeholder would otherwise be sent to the database as a real value. Open the entry in an editor and type the values again instead.

Run again goes through the normal [query execution](running-queries.md), so on a `prod` connection every statement that is not a plain read still needs your confirmation, and placeholders still open the parameter dialog.

Without a selected entry, both commands show "Select a history entry first."

## Saved queries

A saved query is a name, the SQL text and an optional connection. Saved queries are kept in `queries.toml` (hand-editable; see [Settings](settings.md)). Names are unique within a folder.

- **Save** (`query:save`, "Query: Save Query"): saves the text of the active SQL editor. A dialog asks for the name. A new query is bound to the editor's connection. For a tab that was opened from a saved query, the dialog is prefilled with that name; keeping the name overwrites the SQL of that query. If the name is already used, the dialog shows an error and stays open. With no editor SQLer shows "Open a SQL editor first.", and with an empty buffer "Nothing to save."
- **Open** (`query:open-saved`, "Query: Open Saved Query"): opens a picker. Type to filter (every word must be part of `folder/name`, ignoring case), move with Up and Down, press Enter to open the query in a new editor and Escape to close the picker. A query with a connection opens on that connection, otherwise on the active tab's connection, otherwise on the connection selected in the sidebar.
- **Sidebar**: the "SAVED QUERIES" section under the connection list shows every saved query as `folder/name`. Click selects, double-click opens. The selected query has Rename and Delete buttons.
- **Rename** (`query:rename-saved`) and **Delete** (`query:delete-saved`) act on the query selected in the sidebar; without one SQLer shows "Select a saved query first." Delete asks for confirmation.

If `queries.toml` could not be read, or is still loading, saving, renaming and deleting are refused with a message and the file is never overwritten.

## Commands

No command has a default key. Bind them in `keybindings.toml` ([Keybindings](keybindings.md)) or run them from the command palette.

| Command | Title | Does |
| --- | --- | --- |
| `history:open` | History: Open Query History | opens (or activates) the History tab |
| `history:open-entry` | History: Open Entry in Editor | opens the selected entry in an editor |
| `history:run-entry` | History: Run Entry Again | opens the selected entry in an editor and runs it |
| `history:toggle-all-connections` | History: Toggle All Connections | switches between one connection and all |
| `history:clear` | History: Clear History | deletes every entry, after confirmation |
| `query:save` | Query: Save Query | saves the active editor text under a name |
| `query:open-saved` | Query: Open Saved Query | opens the saved query picker |
| `query:rename-saved` | Query: Rename Saved Query | renames the selected saved query |
| `query:delete-saved` | Query: Delete Saved Query | deletes the selected saved query, after confirmation |

## Privacy

The history can contain the literals of your queries. The redaction described above covers statements that look sensitive, but it cannot know every secret. Run `history:clear` to delete all entries; a confirmation dialog comes first and it removes the entries of every connection. There is no setting to switch recording off in the app yet. (The store itself supports `enabled = false`, but the setting is not exposed in the settings page yet.)

Design: [ADR-0021](../adr/0021-query-history-and-saved-queries.md) and [UI workspace](../architecture/ui.md#query-history-and-saved-queries).
