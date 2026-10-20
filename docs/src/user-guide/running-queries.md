# Running queries

A query tab runs SQL on the connection it was opened for. Results appear below the input, one result per statement. The architecture is in [ADR-0024](../adr/0024-query-execution.md).

## Run modes

| Command | Default key (macOS) | Default key (Linux, Windows) | What it runs |
| ------- | ------------------- | ---------------------------- | ------------ |
| `query:run-selection` | Cmd+E | Ctrl+E | The selected text. With no selection it runs the statement at the cursor. |
| `query:run-statement` | Cmd+Enter | Ctrl+Enter | The statement at the cursor. When the cursor is on blank space after a statement, that statement runs. |
| `query:run-all` | Cmd+Shift+Enter | Ctrl+Shift+Enter | Every statement of the buffer, in order. |
| `query:cancel` | Cmd+. | Ctrl+. | Cancels the running query. |

All four are in the command palette as "Query: Run Selection", "Query: Run Statement at Cursor", "Query: Run All" and "Query: Cancel". Statements are split on `;` the way the server reads them: strings, comments, dollar quotes and `BEGIN ... END` bodies are respected, and a `MySQL` `DELIMITER` line changes the terminator and is never sent to the server. A selection is split on its own text, so a selection holding two statements runs both. Starting a run clears the previous results of the tab. A run is refused while another one is running or waiting for input.

## Parameters

A statement with placeholders asks for values before it runs. Recognised placeholders depend on the database:

| Database | Placeholders |
| -------- | ------------ |
| `PostgreSQL` | `$1`, `$2`, ... and `:name` |
| `MySQL`, `MariaDB` | `?` and `:name` |
| `SQLite` | `?`, `?1`, `:name`, `@name`, `$name` |

The dialog has one row per parameter: its label (`$1`, `:id`, `? #1 (statement 2)`), a type and a value. The same `$1`, `?1` or `:name` in several statements takes one value; every plain `?` is asked on its own. Types:

- Auto: `null` (any case) is NULL, a whole number is an integer, a decimal number is a float, `true` and `false` are booleans, anything else is text (kept as typed, spaces included).
- Text, Integer, Float, Boolean (`true`, `false`, `1`, `0`) and Null: the value must fit the type, otherwise the dialog shows the error and does not run.

Values are sent to the server as bound parameters; they are never pasted into the SQL. Cancelling the dialog (Esc or Cancel) runs nothing. On `PostgreSQL` the value is typed: text compared with a `date` column needs a cast, for example `WHERE created > $1::date`, and a Null is bound as an integer.

## Results

Each statement gets its own entry in a strip above the grid, titled with its number and verb, for example `#2 DELETE`. The status line of an entry shows:

- `1,024 rows · 12 ms` for a statement that returned rows;
- `3 rows affected · 4 ms` for one that did not;
- `Error: <message>` when it failed;
- `Cancelled` or `Not run`.

A statement that returns columns shows them in the same grid as an opened table. The run stops at the first error or cancel, and the statements after it show `Not run`. Rows arrive while the statement is still running.

## Cancel

While a run is active, the results area shows "Running…" (or "Cancelling…") and a Cancel button; `query:cancel` does the same. The server-side statement is cancelled, the entry shows `Cancelled`, the rest of the script is not run, and the tab can run again at once. A cancelled statement may already have changed data if the database does not roll it back. Closing the tab or quitting also cancels a running query.

## Production connections

On a connection marked `prod`, SQLer asks before any statement that is not classified as a plain read. This covers `INSERT`, `UPDATE`, `DELETE`, DDL, `CALL`, and anything SQLer cannot classify. A script gets one dialog, before anything runs, that lists each such statement with its exact text and its bound values. Cancel is focused. Declining runs nothing at all, including the reads of the same script. Other connections never ask.

## Limits

- "Read" means SQLer found no write syntax. A function or a view can still write to the database, and SQLer cannot see that, so do not treat the prod dialog as a read-only guarantee.
- SQLer does not open a read-only transaction around your statements.
- On `MySQL`, cancelling can race with the end of the statement and then has no effect.
- The editor is a plain multi-line input for now; syntax highlighting and completion come with the SQL editor.
- Query history is not recorded yet.

See also [Keybindings](keybindings.md) to change the shortcuts, and [Results grid](results-grid.md).
