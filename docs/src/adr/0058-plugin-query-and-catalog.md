# ADR-0058: Plugin query and catalog interfaces

- Status: accepted
- Date: 2026-10-22
- Deciders: SQLer maintainers

## Context and Problem Statement

[ADR-0055](0055-plugin-abi-wit.md) defines the `query` and `catalog` WIT interfaces and
[ADR-0056](0056-plugin-sandbox-and-capabilities.md) gates them with `db.read` and `db.write`. Until P5 the host
answered both with a gate-checked stub ([ADR-0057](0057-plugin-host-runtime.md), extension protocol). A plugin can now
read and, with approval, change data in the user's connections. This is the first place where guest code reaches
user data, so the rules must be fixed before the first plugin uses them: how a connection is named and lent, which
statement may run without asking, who approves, what is capped, and what can leak.

## Decision Drivers

- Default deny: no write syntax, no approval, no run. Classification is fail-safe ([ADR-0020](0020-sql-text-services.md)).
- The host never depends on the app. The app supplies connections and the approval dialog (P7).
- A guest never holds a session, only values.
- A dropped or cancelled call must not strand a session.
- The same approval rule as the AI tools ([ADR-0036](0036-ai-tool-schemas-guards-and-host-traits.md)): every non-Read
  statement asks, and `prod` asks twice.

## Considered Options

- Two traits in the host crate (`DataService`, `QueryApprover`), modelled on the AI tool `SessionSource` and `Approver`;
  the host classifies, asks and runs.
- The app runs guarded SQL behind a `QueryService::run`. Rejected: the property "no non-Read statement runs without
  approval" would depend on app code that the host crate cannot test.
- Streaming or a cursor resource in the WIT. Rejected for now (see Result shape).

## Decision Outcome

Chosen: the first option. Both interfaces are implemented in `sqler-plugin-host` under the `runtime` feature, in
`src/query/` and `src/catalog/`; the deny stubs are removed. `runtime/link.rs` is unchanged because the generated
`add_to_linker` already links both interfaces; only the trait implementation moved.

### How a connection is named and lent

A plugin names a connection by its opaque id. `DataService::connections()` returns the `PluginConnection` values that
plugins may see (`id`, `name`, `DriverId`, optional `Environment`, `read_only`); which connections appear (for example
hiding `prod`) is a policy of the app (P7), not of the host. A connection that is not in the list is `not-found`. The
list never carries a target, host name or secret, and an untagged environment is shown to the guest as `dev`.

For one `query.run` call the host `acquire`s a session, runs one statement and gives the session back through
`release` (default: close and ignore the error). The guest receives only rows. A lent session must never be one on
which the user holds an open transaction; this is the requirement on the app's `DataService`.

The return side has a rule too. An approved `BEGIN`, `SET ROLE`, `SET search_path`, `USE`, a SQLite `ATTACH` or a
temporary table changes the state of the session. `release` must therefore **close the session or reset it completely**
(rolled back, every setting discarded) and must never return it to a pool that the user's tabs draw from. The default
(close) satisfies this. On PostgreSQL and MySQL a close after a result cut short by a row or byte cap may drain the
remaining rows (2 s grace, after which the host stops waiting).

Errors that reach the guest never carry a target. A `DataError::Driver` or a catalog driver error (connect, TLS and
auth messages can name a host, a user or a database) is replaced by a fixed text; only the text of
`DataError::NotAvailable`, which the app writes, passes (clipped).

### Traits

`PluginData` is the one value the app puts into `Services` (`PluginData::new(source)` and optionally
`.with_approver(approver)`); the host reads it with `services.get::<PluginData>()`.

- `DataService`: `connections`, `catalog(connection) -> Option<Catalog>`, `acquire`, `release`.
- `QueryApprover::approve(&WriteRequest) -> Decision` (`Approve` or `Deny`). Dropping the future means not approved.
- `WriteRequest` carries the plugin id and name, the `PluginConnection`, the exact SQL text that would be sent, the
  parameters, the `StatementClass`, the `WriteReason` (`Write`, `Ddl`, `Other`, `Unknown`, `ExplainAnalyze`) and the
  `ApprovalStage` (`Confirm`, `ProdSecond`). The dialog is owned by the host application; the guest cannot render or
  influence it, and it sees the plugin identity and the exact statement.

All of these types are `#[non_exhaustive]`.

### Order of checks in `query.run`

1. Neither `db.read` nor `db.write` in the effective capabilities: `capability-denied`.
2. No `PluginData` in `Services`: `failed`, message `not available in this host`. (Steps 1 and 2 keep the ADR-0057
   gate behaviour of the former stubs.)
3. Validation: SQL over 1 MiB, more than 1000 params, a param that does not parse (temporal or decimal text),
   `max-rows` 0 or `timeout-ms` 0: `invalid-argument`.
4. The connection is not in `connections()`: `not-found`.
5. Plan. When the driver has a SQL dialect the text is split and classified with `classify_each`; otherwise the trimmed
   text is a single `Unknown` statement. No statement or more than one: `invalid-argument` (one statement per call).
   Approval is needed when the class is not Read, or for `EXPLAIN ANALYZE` (it executes the query; this check is a
   deliberate copy of the one in `sqler-ai-tools`, where the function is private; that crate's guard imports `sqler-sql` itself). The text
   sent and shown is the statement body.
6. Gate. No approval needed: `db.read` is required. Approval needed: `db.write` is required (`capability-denied`),
   then a `read_only` connection is `declined`, then no approver is `declined`. The approver is asked with `Confirm`,
   and on a `prod` connection also with `ProdSecond`. Anything other than `Approve` is `declined`. Nothing executes
   before every stage approved. `declined` is a distinct error code, so a plugin can tell a user "no" from a failure.
7. Run: `acquire` (`not-found` or `failed`), execute with the parameters, read under the caps, `release`.

Every non-Read statement asks, also on a non-prod connection. This is stricter than the app's own gate
([ADR-0024](0024-query-execution.md)), which ADR-0056 permits. An unknown statement is not Read, so it needs
`db.write` and an approval.

### Limits

| Limit | Value |
| ----- | ----- |
| SQL text | 1 MiB (`MAX_SQL_BYTES`) |
| Parameters | 1000 (`MAX_PARAMS`) |
| Rows | requested `max-rows`, else the default (1000), at most the cap (10 000), from the ADR-0056 query limits |
| Result bytes | 8 MiB (`MAX_RESULT_BYTES`), counted as cell payload plus 16 per cell; fits under the 64 MiB guest memory |
| Time | requested `timeout-ms`, else the default (30 s), at most the cap (300 s) |
| Catalog items | 10 000 (`MAX_CATALOG_ITEMS`), over it `limit-exceeded` |

The row and byte caps stop reading and set `truncated = true`; they do not cancel the statement for the user. A
timeout calls the driver's cancel handle (2 s grace) and returns `timeout` with no partial rows. A driver error is
`failed`. Only the text of `DriverError::Query` (the database's own statement message) reaches the guest, cut at 512 characters; every other variant, on `execute` and on the stream alike, and any variant added later, becomes a fixed text, because a dropped connection can name the host, user or database.

### Cancellation and the session lease

The borrowed session lives in a `SessionLease`. If the host call is dropped (the enclosing call hits its deadline,
`PendingCall::cancel`, `refresh_grants` or disabling the plugin), the lease cancels the statement and releases the
session on the current tokio runtime. A session therefore never stays lent after the guest is gone.

### Result shape

`columns` hold the name, the WIT data type and a `type-name` (the native type, else the lowercase data-type name);
`rows-affected` comes from the driver's completion event. Temporal and decimal values are passed as their text, and an
unknown value variant as text. The WIT has no streaming and no cursor, so a result is one bounded value and plugins page
with `LIMIT`/`OFFSET`. A cursor resource is a later additive WIT change (ADR-0055 rule).

### Catalog

`catalog` is gated by `db.read`, then by the same missing-`PluginData` rule. `children(connection, parent)` selects the
object kinds by the deepest field of the path that is set: with `table`, indexes and triggers; with `schema`, tables,
views, materialized views, functions, procedures, sequences and types; with `database` only, schemas and extensions;
with nothing, databases, schemas, extensions and roles. An empty `database` or `schema` string means unset. Each kind
is read from the cache first; a miss loads through the connection's catalog worker, which is read-only introspection
shared with the object tree, so a cold cache does not answer empty. Kinds the driver does not support are skipped.
`columns` accepts a path without `table`, looks up the name as table, view or materialized view, and maps `nullable`
and `type-name`. No catalog for the connection is `failed`; an unknown name is `not-found`.

### Approval and the call deadline

The wait for the approver counts against the deadline of the enclosing guest call (for a command, 30 s), and a
timeout means not approved. Writes are therefore practical from commands (user-triggered); a status or render hook
(1 s) cannot wait for a human.

## Threat model

Assets: the data in the user's connections, and the data in their databases that a write could change.

- **Write without consent.** Prevented by the guard: the classifier is fail-safe, one statement per call, every
  non-Read statement needs `db.write`, an approver and an explicit approval of the exact text and params, twice on
  `prod`, never on a read-only connection. A property test checks that nothing non-Read runs without that approval.
- **Approval spoofing.** The dialog is host-owned; the request carries the plugin identity and the exact SQL and
  params. The guest cannot render its own text into it.
- **What "Read" means.** Read means that no write syntax was found. The statement is not run in a read-only
  transaction (ADR-0024). The accepted risk is the same as for the AI tools: a classifier gap, or a function with side
  effects in a `SELECT`, would run without asking. The one-statement guarantee also depends on the splitter of
  `sqler-sql` agreeing with the database about where a statement ends (sqlx-sqlite runs every statement in a string,
  and the MySQL text-protocol fallback accepts several); this is the same trust as in ADR-0036.
- **Exfiltration.** Plugin output is not AI-redacted. Redaction applies only to data bound for a model, and AI-tool
  plugins are covered by P12. Rows can leave the machine only through `net.http` to the hosts the user approved; logs
  are sanitised and local, the UI is local, and `fs.read` is read-only. Hence `db.read` together with `net.http` means
  "may send your data to that host". The grant dialog ([ADR-0059](0059-plugin-capability-grants.md)) shows both
  capabilities, and the user guide (plugins page, "Data leaving your machine") warns about the combination. Hiding `prod` connections from plugins is a policy option of the app.
- **Resource exhaustion.** Rows, bytes, time, SQL size and parameter count are capped; the result fits into guest
  memory.
- **Stranded session.** Covered by the lease; there is a test that cancels during a hanging query.
- **Shared transaction.** The app must not lend a session with an open user transaction.

## Consequences

- Positive: the safety property lives in the host crate and is testable without the app; a plugin never holds a session.
- Positive: the guard matches the AI tools, so one rule covers both.
- Negative: no streaming; large results need paging by the plugin.
- Negative: `Read` has the classifier limits above, and writes are practical only from commands.
- Follow-up: P7 implements `DataService` and `QueryApprover` in the app (connection filter, session pool, dialog).
