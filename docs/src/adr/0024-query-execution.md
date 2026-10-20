# 0024 Query execution: headless pipeline in `sqler-ui`, bound parameters, per-statement results, prod gate

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

From an editor buffer (text, selection, cursor) bound to a connection the user runs the selection, the statement under the cursor or the whole script. Results stream into grids, the run can be cancelled, placeholders are prompted for, and a write on a `prod` connection needs a confirmation. The editor view (T9) does not exist yet, so the pipeline must be testable without it and without GPUI. `sqler-ui` never depends on a driver crate (AGENTS.md §2, ADR-0016), the UI thread never blocks (ADR-0007), and the statement classifier is fail-safe (ADR-0020). AGENTS.md §8 requires an ADR for a new public API.

## Decision drivers

- Testable headless on a real `SQLite` session; views build-only on CI.
- No driver-specific code in the UI layer.
- Parameter values must never be spliced into SQL text.
- A prod write must be visible, exactly as it will run, before it runs.
- Cancel must stop the server-side statement (ADR-0004) and never lose the tab's session.

## Considered options

1. A `QueryRunner` trait in `sqler-ui`, implemented in `sqler-app` (the ADR-0016 pattern).
2. A new headless crate for the pipeline.
3. A headless `query` module in `sqler-ui` that runs on the tab's `Box<dyn Session>`.

## Decision outcome

Chosen: option 3. Nothing driver-specific is needed: the tab already owns a `sqler-driver-api` session (ADR-0017), so `sqler-app` stays unchanged and the layering rule holds. No new crate is added.

**Placement.** `sqler_ui::query` is built without the `gui` feature, so `cargo test -p sqler-ui` covers it. It holds the buffer snapshot, the plan, the parameter form and binding, the run state machine (`QueryTab`), the `QueryJob` and the four commands. The views (results area, parameter dialog, confirmation dialog and a plain multi-line stand-in input until the editor T9 exists) live in `query/view/**` behind `gui`. T9 calls `Workspace::run_query(tab, mode, BufferSnapshot)` and passes its editor element to `query_tab_body`.

**Pipeline.** `BufferSnapshot { text, selection, cursor }` -> `plan` (T3 `split`, `statement_at`, `classify`, `placeholders`) -> parameter form -> prod confirmation -> `bind` -> `QueryJob`. All offsets are UTF-8 byte offsets on char boundaries.

- `Selection` splits the selected text on its own and offsets the ranges; an empty selection behaves like `Statement`. `Statement` takes `statement_at(split(text), cursor)`. `All` takes every `Statement` segment; `Delimiter` and `Trivia` segments never run. Nothing to run is `PlanError::NothingToRun`.
- A driver without a `Dialect` gets the trimmed buffer as one statement of class `Unknown` with no placeholders.

**Commands.** `query:run-selection` (`mod+e`), `query:run-statement` (`mod+enter`), `query:run-all` (`mod+shift+enter`) and `query:cancel` (`mod+.`). The defaults live in the `DEFAULTS` table of `sqler-config` (ADR-0005).

**Binding.** Parameters are bound, never spliced. `bind` rewrites each placeholder to the dialect's native positional marker and passes the values in `Query::params`, which all three drivers already bind.

| Dialect | Rewrite | Values |
| ------- | ------- | ------ |
| `PostgreSQL` | `$k` | one per distinct key, in order of first appearance in the statement |
| `SQLite` | `?k` | one per distinct key, same order |
| `MySQL`/`MariaDB` | `?` per occurrence | one per occurrence (a repeated `:id` repeats its value) |

`k` is the 1-based index of the key among the statement's distinct keys. A statement without placeholders is sent byte for byte. The prompt covers every placeholder of the planned statements once per run: `Positional` and `Named` keys share one value across statements, and every `?` is its own key (`Anonymous { statement, ordinal }`). Each value has a kind (`Auto`, `Text`, `Integer`, `Float`, `Boolean`, `Null`); `Auto` reads `null`, an integer, a finite float and `true`/`false`, and anything else is text. Cancelling the prompt runs nothing. `PostgreSQL` binds typed values, so a text value against a `date` column needs a cast (`$1::date`), and `Null` binds as `int8`; this is documented, not worked around.

**Results model.** One result entry per statement, each with its own `GridSink` and grid (the M1 grid, fed by `grid::ingest` like open data). The entry is `Pending`, `Running`, `Done` (row count, rows affected, duration), `Failed` (message), `Cancelled` or `NotRun`. The statements run in order on one session. The run stops at the first error or cancellation, the remaining entries become `NotRun`, and their sinks are dropped. A new run clears the previous results of the tab; a run while one is running, prompting or confirming is refused with a notice.

**Session on loan.** The workspace lends the tab session to the job (`take_session`) and gets it back in `query_finished`, like the browser jobs. If the tab was closed meanwhile, the returned session is closed. The job never panics and always returns the session. The workspace keeps the session's `CancelHandle`, the job's `stop` flag and every sink's cancel flag. Cancel sets all flags and calls the handle (server-side cancel per ADR-0004, `KILL QUERY` on `MySQL` per ADR-0010). The flags close the gap between statements: `SQLite` resets its cancel flag when `execute` starts, so a cancel that lands between two statements would otherwise be lost. Closing the tab or quitting cancels a running query. Tokio is reached only through `SessionBridge` (ADR-0007).

**Prod gate.** On a connection whose environment is `prod`, a statement whose class is not `Read` needs a confirmation. There is one confirmation per run, before anything executes, listing every such statement with its exact text and bound values, so it also covers run-all. Declining runs nothing, not even the reads. `Other`, `Write`, `Ddl` and `Unknown` all confirm. Other environments never confirm.

**Blind spots.**

- `Read` means "no write syntax found" (ADR-0020). A function or view can still write, and the classifier cannot see it.
- No read-only transaction is opened. `MySQL` DDL commits implicitly, so it would not protect, and a session mode would change what the user's own script does. A read-only connection mode is a later milestone.
- A `MySQL` `KILL QUERY` can race with the end of the statement (ADR-0010).
- `DELIMITER` lines are client-side and never run; the server never sees them.
- On quit, the lent session is not in the set the app closes; it is cancelled and dropped at exit, within the quit bounds of ADR-0016.
- History recording (T11), the editor (T9), grid sort, filter and edit (M4) are not part of this decision.

## Consequences

- Positive: the whole pipeline is covered by unit and `SQLite` integration tests; `sqler-app` and the drivers did not change.
- Positive: values cannot alter the statement text, on any of the three drivers.
- Positive: T9 only supplies a `BufferSnapshot` and an editor element.
- Negative: a run holds the tab session, so the tab cannot run a second statement or open data on it meanwhile; a second run is refused.
- Negative: a prod confirmation is per run, not per statement, so a long script shows one long list.
- Negative: `PostgreSQL` users must cast text parameters themselves.
