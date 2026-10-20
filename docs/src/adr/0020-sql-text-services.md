# 0020 SQL text services: own lexer, fail-safe classification

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

The SQL editor and the query runner need pure text services: split a script into statements with exact byte ranges, find the statement under the cursor, format SQL, classify a statement (read, write, DDL, unknown) and find parameter placeholders. They live in `sqler-sql` (L1) and must not do I/O (AGENTS.md §4). The input is editor text, so it is often half typed, and it can contain client-side syntax such as the `MySQL` `DELIMITER` line. The classifier backs the guard of the AI `run_query` tool (ADR-0003, `run_query (guarded)` in AGENTS.md §2): a mistake there is a safety bug, so anything not proven read-only must not be `read`.

AGENTS.md §2 and the source prompt named `sqlparser` as the parsing library of `sqler-sql`. This ADR deliberately deviates from that: the services are built on a hand-written lexer.

## Decision drivers

- Never fail on half-typed or dialect-specific text; the editor calls these on every edit.
- Exact byte spans for every statement, and tokens that tile the input, so no text is lost or duplicated.
- Fail-safe classification: unknown means not read.
- Linear time, no recursion, no panics on hostile input (one-megabyte strings, deep parentheses).
- No new dependency, no extra licence or `cargo deny` surface.

## Considered options

1. `sqlparser` (AST parser) for all five services.
2. The `sqlformat` crate for formatting, own code for the rest.
3. One hand-written, dialect-aware lexer; every service is a function over its token stream.

## Decision outcome

Chosen: option 3.

**Lexer.** `lexer.rs` is `pub(crate)`. It never fails: an unterminated string, quoted identifier or comment runs to the end of the input and carries a flag. The tokens tile the input exactly and every boundary is a char boundary. Dialects: `PostgreSQL` (nested block comments, dollar quotes, `E''` escapes, `$n` and `::`), `MySQL`/`MariaDB` (`#` and `-- ` comments, backslash escapes, `"` is a string, backticks, `/*!` executable comments), `SQLite` (`[ident]`, `?n`, `@name`, `$name`). `Dialect::MySql` covers `MariaDB` (ADR-0015).

**Whitespace is ASCII only.** Space, tab, LF, VT, FF and CR separate tokens in every dialect. The servers read non-ASCII whitespace and control characters (U+00A0, U+0085, U+3000, U+FEFF) as identifier characters, so the lexer yields them as `Other` (or part of a word), and the classifier turns `Other` into `Unknown`. On `MySQL`, `--` starts a comment only before an ASCII whitespace or control byte or the end of the text (the server tests the next byte with ASCII ctype). A keyword glued to such a character stays glued in the formatter. `SQLite` `[ident]` ends at the first `]` (no doubling); `MySQL` `X'..'` and `B'..'` have no backslash escapes.

**Public API** (pure, re-exported from `sqler_sql`; the types derive `Debug, Clone, PartialEq, Eq` and the enums are `#[non_exhaustive]`):

- `split(dialect, text) -> Vec<Segment>`; `Segment { kind, span, body, terminator }`, `SegmentKind` is `Statement`, `Delimiter` or `Trivia`. The spans tile `0..text.len()`. `body` is trimmed, keeps comments and excludes the terminator.
- `statement_at(segments, offset) -> Option<&Segment>`: the statement that contains the offset, else the nearest preceding one (the cursor sits on whitespace after it), else the first.
- `placeholders(dialect, text) -> Vec<Placeholder>`; `PlaceholderKind` is `Positional(u32)`, `Anonymous` or `Named(String)`. Sets: `PostgreSQL` `$n` and `:name`; `MySQL` `?` and `:name`; `SQLite` `?`, `?n`, `:name`, `@name` and `$name`. Strings, comments, dollar bodies and (on `PostgreSQL`) array slices give none.
- `classify(dialect, text) -> StatementClass` (most severe over the statements) and `classify_each`; `StatementClass` is `Read < Other < Write < Ddl < Unknown`, `is_read_only` is true only for `Read`.
- `format(dialect, text, &FormatOptions) -> Result<String, FormatError>`; `FormatError::Unterminated { offset }` for lexically invalid input.

**Splitting.** A `;` ends a statement only at block depth 0 and, on `PostgreSQL`, at parenthesis depth 0 (psql behaviour). A compound statement (`CREATE TRIGGER`, `PROCEDURE`, `FUNCTION`, `EVENT`, and on `MySQL` `BEGIN NOT ATOMIC`) tracks `BEGIN`/`CASE` ... `END` blocks. A `MySQL` `DELIMITER x` line at a statement start becomes a `Delimiter` segment and changes the terminator; while a custom delimiter is active, only the delimiter ends a statement (as in the `mysql` client) and the word is capped at 16 bytes. Compound statements yield no placeholders (their bodies refer to routine arguments).

**Classification.** Token based. A statement is `Read` only if its lead word is a read verb (`SELECT`, `WITH`, `VALUES`, `TABLE`, `SHOW`, an `EXPLAIN`/`DESCRIBE`/`DESC` whose wrapped statement is itself read, or `SQLite` `PRAGMA` for a fixed list of read-only pragmas without `=`) and a scan of the whole statement finds none of:

- a write word (`INSERT UPDATE DELETE MERGE UPSERT INTO COPY LOAD LOCK SHARE REFRESH`, or `REPLACE` not followed by `(`): `Write`; this catches `SELECT ... INTO`, `INTO OUTFILE`, `FOR UPDATE`, `FOR SHARE`, `LOCK IN SHARE MODE`, CTE-wrapped DML and `EXPLAIN ANALYZE` of a write;
- a DDL lead word anywhere: `Ddl`;
- a string the lexer cannot read the same way under every server mode (see the fail-safe rules below): `Unknown`;
- `CALL DO EXEC EXECUTE`, a `MySQL` `/*!` or `/*M!` executable comment, `:=`, an unterminated token, a stray character, or a function call that is not on the allowlist: `Unknown`.

Fail-safe rules added for mode-dependent lexing and rarely used syntax:

- `MySQL` honours `NO_BACKSLASH_ESCAPES` and `ANSI_QUOTES`, and `PostgreSQL` honours `standard_conforming_strings`; any session can change them. A string literal that contains a backslash is `Unknown` on `MySQL` (any string form, including `@'..'` and `_cs'..'`) and on `PostgreSQL` (any string except `E'..'`, where a backslash is always an escape). On `MySQL`, a `"`-quoted token (or `@"..."`) is `Unknown`, because under `ANSI_QUOTES` it is an identifier. `SQLite` has a single reading (a backslash is always literal), so `Read` stays possible there.
- `MariaDB` sequences: `NEXT VALUE FOR s` advances the sequence and is `Write`; `PREVIOUS VALUE FOR s` is `Unknown`; `SETVAL(..)` and `LASTVAL()` are calls outside the allowlist, so `Unknown`.
- `EXPLAIN [ANALYZE] [(options)] <stmt>` (and `DESCRIBE`/`DESC`) is classified as `<stmt>`: the options are skipped, the wrapped lead decides (`EXPLAIN ANALYZE REFRESH MATERIALIZED VIEW mv` is `Write`, `EXPLAIN ANALYZE REINDEX ..` is `Ddl`, `EXPLAIN ANALYZE EXECUTE p` is `Unknown`), and the whole statement is scanned as well. A wrapped word that is no known lead is `Unknown`, except on `MySQL` (and for `DESCRIBE`/`DESC`) where a short run of names is a table description (`EXPLAIN t`).
- A function call is the default for any word followed by `(`. A word is syntax only if it is in the structural list, is not preceded by `.` (a name after a dot is always a function name, so `public.row(1)`, `db.limit(1)` and `` `db`.cast(1) `` are `Unknown`), and, for words the servers also accept as function names, sits in a position of an explicit table keyed by the previous token: `OVER` and `FILTER` after `)`, `BY` after `GROUP`, `ORDER` or `PARTITION`, `EXPLAIN` as the lead, `MATERIALIZED` after `AS` or `AS NOT`, `RECURSIVE` after `WITH`, `CAST` only with an `AS` and `EXTRACT` only with a `FROM` inside the parentheses, and `PARTITION`, `JOIN`, `LIKE`, `ILIKE`, `IS`, `OFFSET` only after an operand (`)`, a literal, a quoted name, or a plain word that is neither structural nor `ONLY`/`ORDER`/`WINDOW`). After `FROM`, `JOIN`, `LATERAL`, `,`, `ONLY` or an operator the same words are calls: `FROM partition(1)`, `t JOIN join(1)`, `FROM like(1)` and `, lateral partition(1)` are `Unknown`.
- Statements with an `Other` lead (`SET`, `BEGIN`, `COMMIT`, `USE`, `PREPARE`, ...) run the same scans as `Read`: executable comments, mode-dependent strings, unterminated tokens, stray characters, `:=`, write and DDL words and call checks, with `Unknown` on any hit. `SET @a=1 /*! ; DROP TABLE t */` is one statement (the splitter never splits inside a comment) and `Unknown`. `START` other than `START TRANSACTION` is `Unknown`; `SET GLOBAL|PERSIST|PASSWORD` and `@@global`/`@@persist` targets are `Write`; `RESET` is `Other` on `PostgreSQL` and `Write` elsewhere.
- In a `MySQL` script, text after `DELIMITER x` on the same line is accepted only if it is whitespace or a comment. Otherwise the line is ordinary statement text, so it is split and classified, never skipped.

The function allowlist is a positive list of lowercase unqualified names (aggregates, window functions, pure scalar, date and JSON functions, type names with modifiers). It has no prefix rules and never matches a quoted or qualified name. `nextval`, `setval`, `set_config`, `pg_sleep`, `sleep`, `benchmark`, `get_lock`, `pg_advisory_lock`, `pg_terminate_backend`, `pg_cancel_backend`, `pg_read_file`, `lo_import`, `lo_export`, `dblink`, `dblink_exec`, `query_to_xml`, `txid_current`, `pg_notify`, `load_file`, `load_extension` and `last_insert_id` must never be added; each has a test. Lead words map to `Write` (DML, `COPY`, and every lead that can change data, storage or server state: `REFRESH VACUUM ANALYZE CLUSTER LOCK FLUSH KILL ATTACH DETACH OPTIMIZE REPAIR CHECKPOINT NOTIFY INSTALL PURGE`), `Ddl`, `Other` (transaction control, session `SET`, `USE`, `PREPARE`, `LISTEN`, `HANDLER`, ...) or `Unknown` (everything else, including `CALL`, `DO`, `EXEC`, `EXECUTE` and non-read `PRAGMA` on `PostgreSQL`/`MySQL`).

**Known blind spots.** Beyond views, user-defined operators and casts: (1) `PostgreSQL` function overloads can shadow an allowlisted name (a `public.lower(int)` wins over `pg_catalog.lower(text)` for `lower(1)`), and the classifier sees only the name; (2) `SQLite` application-defined functions can override any built-in name; (3) the `MariaDB` sequence forms above are covered only by the rules listed, not by a general model of `MariaDB` syntax; (4) structural words that are reserved on `PostgreSQL` and `MySQL` (`ROW`, `ARRAY`, `LIMIT`, `VALUES`, ...) are always syntax before `(`, although `MariaDB` leaves a few of them (`ROW`) unreserved, and on `SQLite` any keyword may be a function name, which only application-registered functions can exploit; (5) the `EXPLAIN` option skip list is finite, so an unlisted option word makes the wrapped statement look like a table name on `MySQL` (still scanned) and `Unknown` elsewhere. (6) names that run without parentheses are matched by name: on `SQLite` any `pragma_*` table-valued function (`pragma_optimize` writes `sqlite_stat1`) and `sqlite_dbpage` (writable) are `Unknown`, while `dbstat` and `fts5vocab` are read-only and left to the scan; on `MySQL`/`MariaDB` `nextval`, `currval`, `lastval` and a qualified `previous` (the `sql_mode=ORACLE` forms `seq.nextval`) are `Unknown`; other eponymous or loadable modules are not modelled; (7) a `SET` list is `Write` if `GLOBAL`, `PERSIST`, `PERSIST_ONLY` or an `@@global`/`@@persist` variable appears anywhere in it (also as a column or alias name, which is over-cautious), while plain session `SET` stays `Other`. On `MySQL`, DDL commits implicitly, so a read-only transaction does not contain a missed `DROP` or `ALTER`: the classifier is the main guard there, and T8/M5 must treat `Unknown` as a write (confirmation required, never auto-run).

**`Read` means "no write syntax found".** The scan is syntactic. It cannot see a view that calls a volatile function, a `PostgreSQL` user-defined operator or cast, or attribute-notation calls (`t.f`). Callers that must be certain (the AI `run_query` tool, M5/T8) must also run the statement in a read-only transaction (`START TRANSACTION READ ONLY` on `MySQL`, `BEGIN READ ONLY` on `PostgreSQL`, `PRAGMA query_only` on `SQLite`). False `Unknown`s (CTE column lists `x(a)`, `AS t(a)`, UDF calls) only cost an extra confirmation and are accepted.

**Formatting.** A token-based layout on the same lexer: uppercase a fixed keyword list (intersected with the `MySQL` 8.4 reserved words; a word next to `.` is never recased), line breaks before the main clauses and joins, indentation for subqueries, comments kept, strings and quoted identifiers byte for byte, compound statements and `DELIMITER` lines verbatim. Tokens are never added, removed or reordered, which makes the output idempotent and comment-safe. Lexically invalid input gives `FormatError::Unterminated`, never a panic.

**Rejected: `sqlparser`.** The crates.io `max_stable_version` is 0.63.0 (checked 2026-10-20, updated 2026-09-13); it is not a dependency today. It fails on valid dialect SQL and on half-typed text, produces no byte spans, and cannot see the client-side `DELIMITER` line. Its AST also does not prove that a function is pure, so an allowlist would be needed anyway. It stays the candidate for later AST needs (completion scope analysis, T6/M5), where failing on invalid input is acceptable. **AGENTS.md §2 is updated in this PR**: the `sqler-sql` row now reads "SQL text services on an own lexer (statement splitting, cursor lookup, placeholders, fail-safe classification, formatting), dialect handling, DDL generation; `sqlparser` only for later AST needs (ADR-0020)" (it used to say "Parsing (`sqlparser`), formatting, statement splitting, dialect handling"), so the handbook and the code agree.

**Rejected: `sqlformat`.** `max_stable_version` 0.5.0 (checked 2026-10-20, updated 2025-10-09). It does not know dollar quotes, MySQL's `#` or the `-- ` rule, and it gives no guarantee of idempotency or comment safety. We would have to write our own lexer as a test oracle anyway, and a formatter on top of the lexer is small.

**Tests.** Per-dialect case tables, property tests, and a differential test (`sqler-driver-sqlite/tests/sql_classify_differential.rs`) that runs every corpus statement classified `Read` against an in-memory `SQLite` and asserts that schema, data and the change counter are unchanged.

**Dependencies.** None added to `Cargo.toml`. `proptest` and `insta` were already dev-dependencies (ADR-0015).

## Consequences

- Good: one lexer, one token model; five services share one behaviour for strings, comments and dollar quotes.
- Good: never fails, linear time, no recursion, so hostile input cannot hang or overflow the stack (tested with MiB-sized inputs).
- Good: classification fails safe and every dangerous function in the list has a test.
- Good: no new dependency or licence.
- Bad: we maintain a lexer per dialect; new syntax (new quoting forms) needs a code change.
- Bad: classification is syntactic and conservative, so some harmless statements are `Unknown`.
- Bad: `PostgreSQL` `standard_conforming_strings = off`, `MySQL` `ANSI_QUOTES` and `NO_BACKSLASH_ESCAPES` are not modelled; the splitter assumes the defaults, and the classifier fails safe by returning `Unknown` for every string where the readings differ (a backslash, a `MySQL` `"`). Such statements cost a confirmation.
- Bad: an unterminated compound statement, string or comment swallows the rest of the script when splitting.
- Neutral: a later AST need (T6, M5) adds `sqlparser` behind its own ADR.

## Pros and cons of the options

- `sqlparser` for everything: a real AST, but it rejects partial and client-side text, has no spans for splitting and cannot prove purity. Rejected.
- `sqlformat` for formatting: saves a small module, but dialect gaps, no idempotency guarantee and an extra dependency. Rejected.
- Own lexer: chosen; it matches the editor's needs and keeps the safety-relevant classifier under our own tests.
