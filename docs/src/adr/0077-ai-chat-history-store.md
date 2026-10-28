# ADR-0077: AI chat history store

- Status: accepted
- Date: 2026-10-28
- Deciders: SQLer maintainers

## Context and Problem Statement

The AI chat panel ([ADR-0076](0076-ai-chat-panel.md)) keeps its transcript in memory, so a restart loses every
conversation. Operators want the list of past chats and the transcripts back after a relaunch, and want to continue
one. The transcript holds model text, SQL and sample rows from the database, so what is written, how long it stays and
who can read the file are decisions in their own right. [ADR-0039](0039-ai-exploration-session-engine.md) said that
saving transcripts needs a new ADR (redaction, retention, the audit relation). This is that ADR.

## Decision Drivers

- Redaction first: no password, token, API key or connection string with credentials in the file, and no exact SQL.
- Bounded size and age, and a crash loses only a few seconds of chat.
- Writes never block the UI thread; a damaged or newer file never breaks launch.
- No new crate version in the tree; the binary should not grow beyond the module itself.

## Considered Options

- Append-only JSON Lines, like `history.jsonl` ([ADR-0021](0021-query-history-and-saved-queries.md)): rejected, conversations are
  updated, trimmed and deleted by id, which a log makes awkward, and secure deletion needs a rewrite.
- `rusqlite`: rejected, `libsqlite3-sys` has `links = "sqlite3"`, so only one version may be in the tree, and a second
  binding needs its own rationale.
- System SQLite: rejected, the macOS and Windows installers cannot rely on one (same reason as
  `sqler-driver-sqlite`).
- SQLite through the workspace `sqlx` with `sqlite-bundled`, which `sqler-driver-sqlite` already links: chosen.

## Decision Outcome

### Storage and where it runs

`chat-history.db` lives in the configuration directory next to `workbench.toml` and `session.json`
(`ConfigDir::chat_history_path`). The store is the module `sqler_config::chat_history`. `sqler-config` has no runtime
and sqlx needs a tokio context, so `ChatHistory::open` starts one private OS thread (`sqler-chat-history`). The thread
owns a current-thread tokio runtime (`rt`, `time`) and one connection; every sqlx call stays inside its `block_on`.
Callers talk to it over a `std::sync::mpsc` channel. `open` never fails and never blocks. `list` and `load` wait for a
reply (at most 5 s), so the UI calls them on the background executor only. Rejected: running the store in `sqler-ui`,
because every other local store and its file-format tests live in `sqler-config`.

### Schema and migrations

`PRAGMA user_version` holds the schema version (`CHAT_HISTORY_SCHEMA_VERSION`, 1). Tables `conversation` and `entry`
are `STRICT`; `entry` is `WITHOUT ROWID` keyed by `(conversation, seq)` with `ON DELETE CASCADE`. An entry is one JSON
payload with a `kind` column. `MIGRATIONS` is a list where index `i` takes version `i` to `i + 1`; a change to a
payload, or a new `kind`, needs a new migration and a version bump. Migrations run in `BEGIN IMMEDIATE` and re-read
`user_version` under the lock, so two SQLer instances opening a fresh file do not both run the DDL.

### Damaged and newer files

Before opening, the first 16 bytes must be `SQLite format 3\0` and the file at least 100 bytes; `PRAGMA quick_check`
must return `ok`. A file that fails is corrupt: it is renamed to `chat-history.db.corrupt-<unix_secs>` (the `-wal`
likewise, the `-shm` removed), a new history starts, and one notice tells the user. If the rename fails, history is off
and the file is left alone. A file whose `user_version` (header offset 60, checked again after the open) is higher than
this build knows is never opened for writing: history is off for the run and the bytes stay identical. Status and error
texts are fixed strings and never echo file content.

### File permissions and deletion

The database and its `-wal`, `-shm` and `-journal` files are `0600` on unix. The file is created with `create_new` and
mode `0600`; an existing file, and existing side files, are `chmod`ed at open; SQLite's unix VFS gives the side files
the main file's mode. `secure_delete` is on, and `delete`, `clear` and `close` end with
`wal_checkpoint(TRUNCATE)`, so a removed chat does not stay in the WAL. Deleting the file while SQLer is closed clears
the history too.

### What is stored: scrubbing

Scrubbing happens on the worker thread, so every caller goes through it. Order: cut a text to 64 KiB (on a char
boundary, suffix ` [cut]`), then `redact_credentials` (the audit redaction), then three word passes: the userinfo of a
`scheme://user:pass@host` URL; a word with a key prefix (`sk-`, `sk_`, `sk-ant-`, `ghp_`, `gho_`, `github_pat_`,
`glpat-`, `xoxb-`, `xoxp-`, `AKIA`, `AIza`) of 20 or more characters; and the value after a marker word (`passw`,
`pwd`, `secret`, `token`, `apikey`, `api_key`, `bearer`) following `=`, `:` or a space, also after `is`, `are` or
`was`. In JSON, every string and object key is scrubbed, and a string under a key with a marker is replaced whole.
Replacement text is `<redacted>`. There is no regex dependency. Cells of redacted columns are already `<redacted>` in
the tool results and are copied as they are.

The exact SQL is never stored: a `Sql` entry keeps only the display SQL (values already hidden), the class, the
approval, the row count, the truncation and the failure flag. A restored statement therefore cannot be re-run, and it
cannot be opened or copied when its display text had values hidden. Provider state, API keys and the system prompt are
not stored.

Known gaps: scrubbing is heuristic. An unquoted free-text secret with no marker word and no key shape stays in the
text. The user can delete a chat, or all chats, at any time.

### Caps and retention

At most 50 rows per result (the object gets `rows_kept_in_history`), 128 KiB per entry (an oversized entry loses its
`result` and `arguments`, with a note), 64 KiB per text, 80 characters per title. Retention runs after each commit and
deletes the oldest conversation by `updated_ms` while there are more than 100 conversations or more than 32 MiB of
stored payload. The transcript cap of 500 entries ([ADR-0039](0039-ai-exploration-session-engine.md)) still holds:
when the engine drops the oldest entries, the store trims them too (`trim_before`).

### Batching

Writes are fire-and-forget. The first write arms a deadline of one second; one transaction commits all pending
commands at the deadline, after 256 queued commands, on flush and on close. At most about one second of chat is lost on
a hard crash, well within the 5 s allowed. A database error is counted and the thread keeps running. On quit the shell
calls `close` with a 100 ms limit after saving the session.

### Scope and continuing a chat

Only the chat panel (`TabId::CHAT_PANEL`) is persisted. AI session tabs and agent tabs stay unpersisted; tabs stay in
`session.json` ([ADR-0045](0045-workbench-state-persistence.md)). Opening a past chat closes the current one and shows
the saved transcript. Continuing it starts a new turn on a new session on the stored connection, which is a fresh
Exploration session seeded with the saved transcript (`ExplorationSession::with_prior`). No provider state is resumed.
Only the user and assistant text becomes model history, plus the display SQL of each turn as "SQL run earlier" lines,
the most recent ones kept within 32 KiB, starting with a user message, and the first message carries the note that earlier
tool results are not sent again. The chat never starts or sends by itself. If the stored connection is gone, the
transcript is shown read-only with the reason.

### Amendments

This ADR amends ADR-0039 "Persistence" (the chat panel transcript is now stored), ADR-0045 "Not restored" (the chat
transcript is restored from `chat-history.db`, not from `session.json`; the draft is unchanged) and ADR-0076 ("nothing
is persisted"). The audit log is unchanged and still records statements and outcomes, never cell values.

## Consequences

- Positive: chats survive a restart; no new crate version; one place for scrub, caps and retention.
- Negative: scrubbing can miss a free-text secret; the file holds sample rows (up to 50 per result) for as long as the
  chat is kept; a thread and a second runtime exist for the life of the app.
- Neutral: two SQLer instances share the file through WAL and `busy_timeout`; the last writer wins.
