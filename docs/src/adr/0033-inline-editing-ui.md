# 0033 Inline editing: apply session, transaction flag, schema probe, in-place overlay and a single gate

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

ADR-0030 gave the results grid an edit model: editability rules, staged edits, an apply plan and a job that applies it in one transaction. T11 puts a user interface on it: edit a cell, add and delete rows, mark the pending changes, preview the exact SQL, apply or discard, with the production safeguard of ADR-0024. This is a data-modifying path. Five questions are open: which session an apply runs on, how the app knows the session is inside a user transaction (ADR-0030 makes that guard the caller's duty), who finds the schema an unqualified table name resolves to, what the grid shows after a commit, and where the confirmation lives. The new public API in `sqler_ui::grid::edit` and `sqler_ui::workspace` needs an ADR (AGENTS.md §8).

## Decision drivers

- Nothing is written without Apply, and no path (button, keyboard, palette) applies a production plan without the confirmation.
- The preview and the executed statements are the same list; the user's data never alters statement text.
- A query result is edited on the session that read it, so the schema lookup, temporary tables and `search_path` agree with what the user sees.
- The views render state; the flow is headless and tested on a real `SQLite` session.
- No value, statement text or key may reach a log or a panic message through `Debug`.

## Considered options

Session for a query result:

1. The query tab's own session, lent to the apply the way a run borrows it (ADR-0024 `take_session`/`restore_session`).
2. A dedicated session for the apply.

Display after a commit:

1. Update the shown cells in place through an overlay and re-read nothing.
2. Re-run the query.

## Decision outcome

**Apply session.** A query-result grid applies on the query tab's own session (option 1). That is the session the schema lookup ran on, so temporary tables and `search_path` match, and on `MySQL` a temporary table shadows a qualified name only on that session. A dedicated session (option 2) breaks `SQLite :memory:`, cannot see the user's uncommitted rows and can block on the user's own row locks. A table-data tab has no session; it applies on a short-lived session from the `Connector` (ADR-0017 action session, as `open_data` does), which cannot be inside a user transaction. `ApplyJob` takes either (`ApplySession::Lent` or `Connect`) and runs `apply` on it; a lent session is returned to its tab, a connected one is closed. If the connect fails or the stop switch was already set, nothing is sent (`NotStarted`).

**The transaction flag (the guard ADR-0030 requires).** The workspace keeps one flag per query tab, "in a user transaction as far as the app knows". It is set and cleared by `tx_effect(dialect, sql)`, which reads the significant tokens of each statement a run executes: `BEGIN`, `START TRANSACTION`, `SAVEPOINT`, `COMMIT`/`ROLLBACK ... AND CHAIN` and `SET autocommit = 0` begin; `COMMIT`, `END`, `ROLLBACK` without `TO` and `PREPARE TRANSACTION` end. `autocommit = 0` (or a value that cannot be read) is sticky on `MySQL`: the next transaction starts as soon as one ends, so `COMMIT` and `ROLLBACK` keep the flag, and only a finished `autocommit = 1` or a lost or closed session clears it. In a script, the flag stays set after `SET autocommit = 0` unless a later `autocommit = 1` follows. It errs toward "in transaction": a begin sets the flag also when the statement fails or is cancelled, an end clears it only when the statement finished or failed, and a statement it cannot read leaves the flag as it was. After an apply the flag is set when the apply did not start or when a rollback or commit left the session in doubt. It is dropped when the session closes or is lost. An apply is refused while the flag is set (the user clears it with `COMMIT` or `ROLLBACK`, or with `SET autocommit = 1` when autocommit is off). It cannot see a `COMMIT` inside a procedure, a function or a `DO` block, or a `MySQL` implicit commit; those leave the flag set, which is the safe direction. A grid whose tab has no session cannot be in a transaction.

**The schema probe.** `check_editable` trusts `resolved_schema` (ADR-0030, rule 4, N1). `SchemaProbe::for_sql` returns a probe only for a plain single-table `SELECT` that names its table without a schema. The probe is one fixed read-only `SELECT` per dialect (`to_regclass` on `PostgreSQL`, `pragma_table_list` ordered `temp`, `main`, then attach order on `SQLite`, `DATABASE()` on `MySQL`); the table name is a bound parameter and is never part of the text. The query job runs it on the run's own session right after the statement finished, only if the run is not being stopped, and reports `RunEvent::Resolved`. Any error, a missing row or a `NULL` gives `None`, and `None` makes the result read-only (fail-safe). The cost is one extra catalog read per eligible `SELECT`, on the user's session; this is documented in the user guide. `pragma_table_list` needs `SQLite` 3.37 or later; on an older library the lookup fails and the result is read-only.

**In-place overlay.** After `Committed` the staged edits move into an overlay: updated cells show the written value, deleted rows are marked as deleted, and inserted rows appear below the model's rows. The `GridModel` is never changed and nothing is re-read, so a trigger, a default or a generated column is not shown, and the shown value is the one that was written. Rows the apply changed are read-only until the data is reloaded (a second edit would be found by stale original values), and fetching more rows is refused while rows sit below the model's (the next page would shift them). The alternative, re-running the query, discards the user's scroll position and sort, may return other rows, and costs a query; it can be added later without changing this model.

**One gate.** Every apply goes through `Workspace::request_apply`. It builds a new plan on every attempt, derives `production` from the connection of the tab, never from the context the grid was given (the stricter of the tab's connection and the saved connection it came from; a tab that cannot be found, or an environment label this code does not know to be safe, counts as production; a connection without a label does not, as in ADR-0024), and asks `ApplyPlan::needs_confirmation`. If a confirmation is needed it stores one `EditConfirm` (which owns the plan) and shows every statement and value; `confirm_apply` takes it together with the grid's plan at that moment (a plan that differs, because the user edited while the dialog was open, writes nothing), derives `production` again and calls `plan.confirm(production, true)`. A second confirm finds nothing. Declining writes nothing. Because `ApplyJob` holds a `ConfirmedPlan`, no job can exist without passing that step. The refusals are in the same place: no tab, a run in progress, an apply in flight, an open transaction, no session, a plan that fails to build, and the unknown outcome of a previous `COMMIT` (Apply stays blocked until Discard). An apply job that ends without a report (its task died) counts as an unknown outcome too, and the tab loses its session. A transaction flag that blocks Apply also blocks a re-run while changes are pending, so the user discards the changes, runs `COMMIT` or `ROLLBACK` (or `SET autocommit = 1` with autocommit off), and edits again.

**Other decisions.**

- Binary cells are read-only in this milestone. Hex editing is deferred.
- There is no per-statement progress: `apply` has no callback. The UI shows "Applying N statements" and a Cancel button, which sets the stop flag and sends the session's cancel handle.
- A query with pending edits is not re-run; the user gets a notice to apply or discard first. Closing a tab and quitting with pending edits ask first.
- There is no undo stack. "Revert selection" and "Discard" cover it.
- Sort and filter apply to the values as read; new rows stay at the end. Edits are keyed by the model row (ADR-0030), so they land on the right row on a sorted or filtered view.
- `Debug` of `Query`, `StagedEdits`, `ApplyPlan`, `ConfirmedPlan`, the planned and previewed statements, `ApplyOutcome`, `GridEditor`, the cell editor, the display cell, the job and the grid tab is written by hand and prints counts, lengths and kinds only. T11 code never formats a value, a statement or a key with `{:?}`. `sqler_core::Value` itself keeps its derived `Debug` (it is the data type of the whole workspace); the types that hold values around it do not expose it.

## Consequences

- Positive: an edit of a query result uses the session the result was read on, so the editability decision and the write agree about which table is meant.
- Positive: the production gate and the transaction guard each have one place, and a headless test per path.
- Positive: a failed lookup, a view, a join or a shadowing temporary table gives a read-only grid with a stated reason, never a write to the wrong table.
- Negative: an apply holds the tab's session for the whole transaction, like a query run, so a run cannot start meanwhile.
- Negative: the transaction flag is a heuristic over statement text; it errs toward refusing an apply the user could have made, and the way out is `COMMIT` or `ROLLBACK` (or `SET autocommit = 1` with autocommit off).
- Negative: the grid shows what was written, not what the server stores, until the data is reloaded.
- Negative: one extra read-only statement runs after each eligible `SELECT`.
