# 0021 Query history and saved queries: JSON Lines history, versioned TOML saved queries

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

The SQL editor needs a query history that can be searched and a list of saved queries, and a history tab (T11) will show both. Both are UI-independent stores in `sqler-config` (layer L1, no async). The history gets a line on every executed query, so an append must be cheap and must not rewrite the file. Saved queries are written rarely and the user may edit them by hand. Both files can hold what the user typed into SQL, so the privacy limits need to be stated. This ADR follows ADR-0005 (config files) and ADR-0011 (connection store format).

## Decision drivers

- Append is cheap, and safe when several tabs record at the same time (single process).
- Search stays fast at the retention cap.
- A corrupt line or a file from a newer SQLer is reported and never silently discarded.
- Saved queries are hand-editable and typo-safe, like the other config files.
- No password, bound parameter or error text is recorded.

## Considered options

History format:

1. JSON Lines, one versioned entry per line, appended with `O_APPEND`.
2. One TOML file, rewritten on each query.
3. An embedded database such as `SQLite`.

History location:

1. The config directory, next to the other files.
2. A separate data or state directory.

## Decision outcome

Chosen: JSON Lines in the config directory for history, and versioned TOML in the config directory for saved queries.

### Location

Both files live in the config directory of ADR-0005 (the table of directories per OS is in the [Settings](../user-guide/settings.md) page):

- `history.jsonl`, opened with `ConfigDir::open_history(options)`.
- `queries.toml`, read with `ConfigDir::load_saved_queries()` and written with `ConfigDir::save_saved_queries(&queries)`.

A separate data or state directory only differs on Linux and needs a second resolver, so it is left for a later ADR.

### History format v1

One UTF-8 line per entry, ended by `\n`, keys in this order:

```json
{"v":1,"started_ms":1760950000123,"connection":"billing","duration_ms":42,"outcome":"ok","rows":10,"sql":"select 1"}
```

- `started_ms` is the start time in Unix epoch milliseconds (`u64`); a time before 1970 is stored as 0.
- `connection` is a connection ID (the grammar of ADR-0011 and ADR-0012). `outcome` is `ok`, `error` or `cancelled`.
- `rows` is left out when unknown. `"truncated":true` is present only when the SQL was cut.
- Unknown keys are errors for that line. A `v` of 0, a missing or invalid `v` is a malformed line; a `v` greater than 1 is a newer-version line.
- The API has no field for bound parameters or error messages, so they cannot be recorded.

### Retention

- The default cap is 10,000 entries (`DEFAULT_HISTORY_CAP`). The caller sets `max_entries`; it is clamped to 1 to 100,000. The history can be turned off with `enabled = false`.
- SQL longer than 256 KiB (`MAX_HISTORY_SQL_BYTES`, 262,144 bytes) is cut at a character boundary and marked `truncated`.
- Appending never rewrites the file. When the file holds more than `max_entries + max(max_entries / 10, 100)` entry lines (bad lines are not counted), the next record compacts it: the file is rewritten atomically with only the newest `max_entries` entries.
- Memory always holds at most `max_entries` entries. Lowering the cap trims memory at once and the file at the next compaction.
- Search splits the text on whitespace; every term must be a case-insensitive substring of the SQL. It can filter by connection and returns the newest entries first, up to a limit. It is a linear scan, which is fast enough at the cap (checked by a test with 10,000 entries).
- Fuzzy matching was rejected: it needs a dependency and gives noisy matches on SQL.

### Corruption policy

Only a read failure of the file is fatal. A bad line (not valid UTF-8 or JSON, wrong types, an unknown key, a bad connection ID) or a line of a newer version is reported as a `HistoryIssue` with its 1-based line number and kind. The message never repeats the line content. The other lines still load. Bad lines are kept byte for byte when the file is compacted, so nothing is silently discarded; only `clear` drops them. Load never rewrites the file.

A crash can tear the last line, because appends are not flushed to disk. When the file does not end in a newline, the next append writes a newline first, so the torn line is never glued to a new entry. The torn line is reported like any bad line.

### Saved queries format v1

```toml
version = 1

[[query]]
id = "q-1"
name = "Open invoices"
folder = "billing/reports"
connection = "billing"
tags = ["invoices", "monthly"]
sql = """
select * from invoices where paid = false
"""
```

- `version`, and per query `id`, `name` and `sql`, are required. `folder`, `connection` and `tags` are optional. Unknown keys are errors.
- The order of the `[[query]]` entries is the order of the store. A new query is appended.
- The ID follows the connection-ID grammar. New IDs are `q-<n>` with `n` one more than the highest existing `q-<n>`. The ID never changes, not even on rename.
- The name is trimmed, not empty, and unique within its folder. A query without a folder counts as one folder.
- A folder is `/`-separated segments, each trimmed and not empty, written as `a/b`. Folders are path strings; there are no folder entities. An empty folder means no folder.
- Tags are trimmed and not empty; duplicates are removed and the order is kept.
- The `connection` binding is checked against the ID grammar only. A binding to a connection that no longer exists is kept.
- A `version` greater than 1 is rejected as "written by a newer SQLer". Errors are `ConfigError::Parse` (syntax and types, no echo of values) or `ConfigError::Invalid`, which names the file and the entry, for example `query[2] "Open invoices": ...`. A bad file is never overwritten by a load.
- The file is written whole and atomically (temporary file, flush, rename), like `connections.toml`, so comments and key order of a hand-edited file are not kept.

### Concurrency

`HistoryStore` is `Send + Sync`: an internal lock lets tabs on several threads record at the same time. The calls block; async callers use `spawn_blocking`, as with `SecretStore`. Each append opens the file in append mode, writes one buffer and closes it. Only one SQLer process is supported. Two running instances are not coordinated, and a compaction in one can lose lines that the other appended meanwhile. `SavedQueries` is a plain value store (load, change, save); the last save wins.

### Privacy

- History holds the SQL as typed, and that can include literals such as names, e-mail addresses or an inline password. Saved queries hold the SQL the user stored.
- Bound parameter values, passwords from the connection profile and error texts are never recorded; the API cannot take them.
- Before a statement is stored it goes through a credential redactor, which is always on. It is conservative by design (it over-redacts) and it is a heuristic, not a guarantee. The text is first cut to the size cap, so the work and the memory are bounded; a construct the cut leaves open is hidden to the end. Whether a statement is sensitive is decided on the raw text with case-insensitive matching of a broad marker list. Substrings, matched token by token as written (whitespace is not removed): `passw` (password, passwd, `password_hash`), `pwd`, `secret`, `token`, `credential`, `apikey`, `api_key`, `authorization`, `auth_`, `authenticat`, `oauth`, `bearer`, `identified`, `_key`, `salt`, `encrypt`, `decrypt`, `crypt(`, `sslkey`, `ssl_cert` and similar. Whole words only: `pw`, and `key` (as in `pgp_sym_encrypt(x, key)` or SQLCipher `ATTACH ... KEY`, but not after `PRIMARY`, `FOREIGN`, `UNIQUE` and similar). Multi-word markers (`private key`, `api key`, `identified by`) are matched on whitespace-normalised text. A URL with user info (`://` together with `@`) also counts. Bare `auth`, `ssl`, `cert`, `otp` and `hash` are not markers: they match harmless text (`author`, `class l`, `NOT price`), a trade-off accepted on purpose. No lexing is involved, so a marker inside a comment, a backtick or double-quoted name or a literal counts.
- In a sensitive statement, every `'...'`, `"..."` and dollar-quoted literal, every backtick identifier, and every comment (including `MySQL` `/*! ... */` executable comments) becomes `'<redacted>'`. Literals with JSON, YAML, `key=value` or URL credentials are covered because all literals are. Other statements, including their comments, are stored unchanged.
- The statement is lexed under all combinations of backslash escapes in quotes (`MySQL`, `E'..'`), nesting or non-nesting block comments, `#` comments, `--` comments with or without a following space, and dollar quoting on or off (tags follow PostgreSQL: letters of any script, digits and `_`). The union of the ranges of all readings is redacted. A quote, backtick, comment or dollar quote that any reading leaves unterminated hides everything to the end of the statement.
- Known gaps: an unquoted secret such as Oracle `IDENTIFIED BY pw`; password hashes written as bare hex (`MSSQL` `0x...`); a secret behind a column or variable name that carries no marker; an encryption key passed as a literal to a function whose name carries no marker and without a `key` word. A property test plants a secret in many literal and comment positions after a marker, among dangerous fragments (including dollar tags with non-ASCII letters), and checks that it never reaches the output.
- The user can turn the history off (`enabled = false`) and delete it with `clear` (all) or `clear_connection`. The files are plain text with the permissions of the config directory.
- Do not sync the config directory to a public place, for example in dotfiles, without excluding `history.jsonl` and `queries.toml`.

### Dependency

`serde_json` becomes a direct dependency of `sqler-config` for the history lines. It was already in the lock file through other crates. The checked version and licence are in [Dependencies](../dependencies.md).

## Consequences

- Good: a record is one `O_APPEND` write, with no rewrite of the file.
- Good: bad lines and future-version lines survive until the user clears the history.
- Good: saved queries are readable, diffable TOML in the same style as the other config files.
- Bad: no fsync on append, so a crash can lose or tear the last line.
- Bad: the redactor is heuristic, over-redacts sensitive-looking statements, and the history can hold literals.
- Bad: two app instances can lose lines during a compaction.
- Neutral: history options are passed in by the caller; storing them in `settings.toml` and the UI are T11.

## Pros and cons of the options

- JSON Lines: cheap append, line-wise recovery, simple parse. Not meant for hand editing. Chosen.
- One TOML file: easy to read, but every query would rewrite the whole file and one bad byte hides all entries. Rejected.
- An embedded database: indexes and transactions, but a heavy dependency for a linear scan over 10,000 short entries, and not hand-recoverable. Rejected.
- Config directory: one place for all files, one resolver. Chosen.
- Data or state directory: more correct on Linux, but a second resolver for little gain. Left for a later ADR.
