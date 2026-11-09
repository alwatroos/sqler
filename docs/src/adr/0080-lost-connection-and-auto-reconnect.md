# ADR-0080: Lost connection and automatic reconnect on a run

- Status: accepted
- Date: 2026-11-09
- Deciders: SQLer maintainers

## Context and Problem Statement

A query tab keeps one session for as long as it is open. When the server restarts, drops an idle
connection, or the network goes away, the next run fails with an ordinary query error and the user has
to find the Reconnect action by hand. Drivers cannot say "the
connection is gone" apart from "the statement is wrong", so the app has nothing to act on. The hard part
is the retry: running a statement again may run a write twice.

## Decision Drivers

- A lost connection must be distinguishable from a query error in `sqler-driver-api`, without breaking
  the DuckDB engine or plugin drivers.
- Never repeat a write that may have run, and never skip the production confirmation.
- Reuse the reconnect path (connect request, secrets, tunnels, host-key prompt, failure text).
- No extra round trip on a healthy run.

## Considered Options

- Reconnect inside `QueryJob`.
- A ping before every run.
- A new `DriverError` variant that drivers fill, and a workspace step that reconnects and decides.

## Decision Outcome

Chosen option: a new variant `DriverError::ConnectionLost { message, sent }` on the `#[non_exhaustive]`
enum, plus a workspace step that starts the reconnect of the tab and runs the statements again only when
that is safe.

**The variant.** `message` is a short text written by the driver (a server message, an `io::ErrorKind`
text or a fixed text), never a target, host or secret. `sent` is `false` only when the statement
certainly did not execute. `DriverError::is_connection_lost` and `DriverError::lost_before_send` read it.
A driver reports the loss from `execute` or from the stream.

**What counts as lost, per driver.**

| Driver | Lost | `sent = false` |
| ------ | ---- | -------------- |
| PostgreSQL | I/O error, worker crash, closed pool, SQLSTATE class `08`, `57P01`, `57P02`, `57P03` | the failure happens while preparing (Parse/Describe) |
| MySQL, MariaDB | I/O error, worker crash, server codes 1053, 1927, 4031, 2006, 2013 | the failure happens in `COM_STMT_PREPARE` |
| SQLite | the worker thread stopped | never |
| SQL Server | any non-server error, server errors of severity 20 or more | the session was already dead before the request was sent |

Every unclear case is `sent = true`. The text-protocol paths of the MySQL driver stay `sent = true`.

**The run.** `QueryJob` reports the position of the lost statement and whether it was sent. The workspace
puts the dead session back in its tab, starts the existing reconnect of that tab, and keeps the statements
of the run. When the new session is stored:

- The first statement was not sent, or it is a read (the fail-safe classification of `sqler-sql`): the
  same bound statements run again, with the notice "Connection restored; running the query again." On a
  `prod` connection a plan with a statement that is not a plain read opens the confirmation again; the
  retry never skips it.
- Otherwise, or when the loss was at a later statement of a script, nothing runs and the notice says
  "Connection lost and restored; run again."
- The session had an open transaction (as `workspace/tx.rs` follows it): nothing runs and the notice says
  the transaction was lost and its uncommitted changes are gone.
- One attempt per run: when the re-run loses the connection too, the tab only gets a notice. A failed
  reconnect shows "Could not restore the connection: ..." with the connect failure text, and the tab stays
  usable. After a user cancel, a loss does not reconnect; the next run does.

### Consequences

- The variant is additive: DuckDB, plugin drivers and every `_` arm compile unchanged.
- The sqlx drivers cache prepared statements. A statement that was run before fails at execute, not at
  prepare, so it counts as `sent = true`: a write is then not repeated and the user runs it again. This is
  the conservative direction.
- Session state is lost with the connection: temporary tables, `SET` and `USE`. The automatic re-run
  happens without it.
- A re-run waits for the reconnect; a run requested meanwhile gets "The connection is being restored; wait
  a moment."
- The DB tests of each server kill the session on the server side and check the classification.

## Pros and Cons of the Options

### Reconnect inside `QueryJob`

- Good, because the run would not leave the job.
- Bad, because the connect path, secrets, tunnels and host-key prompts live behind `Effect::Connect`, and
  the job has no access to them.

### A ping before every run

- Good, because a dead session is found before the statement is sent.
- Bad, because it adds a round trip to every run and still races with a drop after the ping.
