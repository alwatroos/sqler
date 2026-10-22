# 0039 AI exploration session engine

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

ADR-0034 gave the provider API, ADR-0036 and ADR-0037 the tool set with its guards (approval per statement, row and time caps, redaction, untrusted-data envelope, audit). Nothing yet connects them: a user question must become a bounded loop of model steps and tool calls on one connection, with approvals, limits, cancellation and a transcript the UI can show. This ADR adds the module `sqler_ai_tools::session` (L3, headless, no UI types).

## Decision drivers

- Every guard stays in `ToolSet`; the session cannot weaken it.
- The loop is bounded in steps, tool calls, tokens and time, and detects a model that repeats itself.
- Cancel at any await point leaves the session usable.
- No secrets in the session; nothing is written to disk.
- Testable without network: `ScriptedProvider` and a SQLite source.

## Considered options

1. A `ToolSet::refuse` that builds results for calls the session refuses (limit, loop, cancel) through `finish`.
2. The session leaves refused calls out of the provider history and lists them in the transcript as "not run".
3. The session builds its own `ToolResult` values for refused calls.

## Decision outcome

Chosen: option 2. The `ToolResult` source guard (ADR-0037, `tests/audit_structure.rs`) allows construction only in `toolset.rs`; option 3 breaks it, and option 1 edits the guarded files and widens the guard test. The model only ever sees `report.result` from `ToolSet::call_detailed`; the session never wraps or redacts on its own.

### State machine

`ExplorationSession` is bound to one connection and has seven states: `Idle`, `Streaming`, `ToolCallPending`, `AwaitingApproval`, `Done`, `Cancelled`, `Failed`.

- `Idle` -> `Streaming` when `send` starts a model step.
- `Streaming` -> `ToolCallPending` when the step ends with tool calls; -> `Done` when it ends without; -> `Failed` on a provider error; -> `Cancelled` on cancel.
- `ToolCallPending` -> `AwaitingApproval` while the host approver decides (set by the session approver); back to `ToolCallPending` when it answers; -> `Streaming` for the next step; -> `Done`, `Cancelled` when a limit, loop detection or cancel ends the turn.
- `Done`, `Cancelled`, `Failed` are final for the turn. The next `send` starts again from `Streaming`; the session stays usable.

Each change emits `SessionEvent::State`; a turn ends with `TurnEnded(TurnOutcome)` where the outcome is `Completed`, `LimitReached(LimitKind)`, `LoopDetected`, `Cancelled` or `Failed(SessionError)`.

### The loop and its bounds

One `send(text)` is one user turn. Each model step streams through `AiProvider::stream`; the tool calls of the step run in order through `ToolSet::call_detailed`; the reports go back to the model; the loop repeats until the model ends its turn. `SessionLimits` defaults:

| Limit | Default | Scope |
| ----- | ------- | ----- |
| `max_model_steps` | 10 | per user turn |
| `max_tool_calls` | 25 | per user turn |
| `token_budget` | 200,000 | whole session |
| `turn_timeout` | 300 s | per user turn |
| `max_identical_calls` | 2 | per user turn |
| `retry` | see below | per model step |

The limits are checked before every model step; the token budget is also checked on every `Usage` event and cuts the stream mid-way (the stream is dropped, so the provider request ends). The defaults are assumed; they are not in `ai.toml` yet. The system prompt (`system_prompt`, version `SESSION_PROMPT_VERSION`) names the bound connection and tells the model to answer schema questions with catalog tools only, to use `run_query` and `sample_rows` for data questions with small `LIMIT`s, not to retry a denied statement, and to treat text between the untrusted-data tags as data.

### Approval path

The host builds the `ToolSet` and passes an optional `Approver`. When there is one, the session wraps it in a `SessionApprover`: it reports `ApprovalRequested`, awaits the host decision, adds the wait time to the turn's `waited` total and emits `ApprovalResolved(Some(decision))`, and returns the decision unchanged. A drop guard emits `ApprovalResolved(None)` if the wait is dropped (cancel). Approval wait time does not count toward `turn_timeout`, the same as the per-call deadline in ADR-0036. With no approver the tool set itself refuses with `no_approver`; production connections ask twice (Confirm, ProdSecond) as in ADR-0036. The wrapper cannot approve anything by itself.

### Retry policy

A stream error before any text or tool call is retried when it is `RateLimited` or `Network`, at most `max_retries` (2) times, with delay `retry_after` if the provider gave one and it is at most `max_delay` (10 s; a longer one means no retry), else `min(base_delay * 2^(attempt-1), max_delay)` with `base_delay` 1 s. Each retry adds `Notice::Retrying`. The step count does not change. Other errors (`Auth`, anything after text, a stream that ends before `Done`) end the turn with `Failed(Provider(..))` and `Notice::ProviderError`.

### Loop detection

A `RepeatGuard` keys calls by tool name and canonical arguments (JSON with object keys sorted recursively; the raw text when the arguments do not parse). The call that would be the `max_identical_calls + 1`-th in a turn is not run, `Notice::LoopDetected` is added and the turn ends with `LoopDetected`.

### Refused calls are left out of the history

A call the session itself refuses (limit, loop, cancel before approval) is not dispatched, so the tool set produced no result for it, and the session may not invent one (see decision). Such calls are listed in the transcript as `ToolCallStatus::NotRun(Limit | Repeated | Cancelled)` and the turn ends. The provider history gets only the assistant message with the dispatched calls and one tool result per dispatched call, so every call in the history has a result. The text of the assistant message is kept. Risk: a provider could dislike text that mentions calls that are absent; low, because the call parts are simply missing, and a test checks that the next request is well-formed.

### Cancellation

`SessionCancel` is an atomic flag plus a list of wakers. `cancel()` sets the flag and wakes every waiter. The session races it at every await: the stream read, the retry sleep, the tool call, and (through the dropped future) the approval wait. Dropping the `send` future cancels too: a drop guard marks `Cancelled`, adds `Notice::Cancelled` and emits `TurnEnded(Cancelled)`. A user message left by an aborted turn is merged into the next one (`previous + "\n\n" + new`), so the history stays valid. Cancelling the tool call drops the call future, which cancels the driver as in ADR-0036 and, through the tool set's drop guard, records one `cancelled` audit entry (ADR-0037). The transcript then says what is known: a cancel while the approver is still asked is `NotRun(Cancelled)` (nothing was sent); a cancel after that is `ToolCallStatus::Cancelled`, which means the call may have run, so the transcript never claims it did not. No `Sql` entries are added for it; the audit entry holds the statements flagged as sent.

### Transcript and re-run

The `Transcript` is a typed in-memory list: `User`, `Assistant`, `ToolCall`, `Sql`, `Notice`. It is built from the redacted `CallReport` (arguments are the credential-scrubbed audit arguments; the result is the parsed untrusted-data content). Each executed statement gets a `SqlEntry` with the connection, a private exact `sql` (read only by `rerun()`; `Debug` and the events never show it) and a `display_sql` (the audit text, or the credential-redacted text beyond the audit cap), the class, whether it was approved, the row count, truncation and failure. `SqlEntry::rerun()` returns a `RerunRequest { connection, sql }`; the UI feeds it to the normal run flow (ADR-0024) with its usual confirmation. Re-run sends nothing to the provider and runs nothing by itself.

### Persistence

None. The transcript lives as long as the session object (the tab). Nothing is written to disk, and the session holds no key or secret: the API key stays in the provider. If saving transcripts is ever added, it needs a new ADR (redaction, retention, the audit relation).

### Host duties

The host (the session UI, T13) builds the `ToolSet` over a `SessionSource` that lends only the bound connection, sets `with_audit(.., Surface::Session)` and passes the approver. The session cannot check this.

## Consequences

- Positive: guards live in one place; a limit, loop or cancel always ends the turn cleanly; the UI gets events and a typed transcript.
- Negative: the tool set is rebuilt once per session by `with_approver` (the definition cache is dropped once); refused calls have no entry in the model's history.
- Follow-ups: map the limits to `ai.toml`; the session UI.
