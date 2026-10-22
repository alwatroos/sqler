# 0053 AI session tab and approvals

- Status: accepted
- Date: 2026-10-22

## Context and problem statement

The exploration session engine ([ADR-0039](0039-ai-exploration-session-engine.md), `sqler-ai-tools`) holds a
conversation with a model about one connection and asks an `Approver` before every statement that is not a
read. The app needs a place to show that conversation, an approval card that shows the exact SQL, and a
way to answer for production connections without a second, weaker confirmation. The workbench and the AI
surfaces are fixed by [ADR-0042](0042-workbench-layout.md) and [ADR-0050](0050-ai-surfaces-in-the-workbench.md);
the query run gate by [ADR-0024](0024-query-execution-and-safety.md); the connect pipeline by
[ADR-0016](0016-connect-pipeline-and-database-sessions.md); the UI/tokio bridge by
[ADR-0007](0007-desktop-binary-and-ui-tokio-bridge.md).

## Decision drivers

- Every state and flow must be testable headless, without GPUI.
- The statement the user approves is the statement that runs: the card shows the exact SQL.
- A production connection needs the same second confirmation as a manual run, not a copy of it.
- No change to `sqler-app`; connections open through the binary's existing connect pipeline.
- Nothing from the conversation is written to disk by the tab.

## Considered options

1. A long-lived actor task that owns the session and talks to the UI by messages.
2. A new pinned tab kind whose state lives in the `Workspace`; the session is lent to a task for one turn
   and returned (the pattern of query runs).

## Decision outcome

Option 2. Option 1 lost because the state would live off the `Workspace`, and the headless tests and the
existing lend pattern would not fit.

| Topic | Decision |
| ----- | -------- |
| Tab kind | `TabKind::AiSession`, pinned, titled `AI: {connection}` (or `AI: {object}`). One session per tab; several tabs per connection are allowed. `TabKind::uses_run_gate()` tells the tab kinds that own a run gate. |
| Turn | The idle session sits in the tab state. A turn moves it into `Effect::RunAiTurn`; the view runs it on tokio through `SessionBridge::spawn`; it comes back as `HostMsg::TurnDone`. A lost task (dropped sender) ends the turn as failed. |
| Host channel | One ordered `tokio::mpsc` channel per tab carries everything the session tells the UI: session events, approval asks, UI-tool calls and the end of the turn. The view drains it into `Workspace::ai_session_message`, so ordering is exact. |
| Database access | `TabSource` implements the T7 `SessionSource`. It opens sessions on the connect pipeline through `SessionBridge::connector()`, keeps one idle session, records the in-flight `CancelHandle`, and is shut on close or quit. No `sqler-app` change. |
| Approvals | A reusable `ai::approval` module (T17 reuses it): `ChannelApprover` implements the T7 `Approver` over the channel, an `ApprovalQueue` holds the reply senders. The `Confirm` stage shows an approval card with the exact SQL and **Deny** focused by default. |
| Production | The `ProdSecond` stage opens the existing ADR-0024 gate: a `QueryTab` under the AI tab's id in `Workspace::queries`, driven by `QueryTab::request`, rendered by the existing overlay and confirm dialog, answered by `confirm_query` / `dismiss_query_prompt`. Each of those has one AI branch; the gate is not forked. A stray confirm cannot answer a `Confirm` card, and the approve/deny commands act on `Confirm` asks only. The gate answers only the ask it was opened for. |
| Answers are bound to the ask | `ApproveStatement(tab, id)` and `DenyStatement(tab, id)` carry the ask id (`PendingApproval.id`). `ApprovalQueue::answer_id` answers only when the front ask has that id and stage; a stale id is a no-op with a notice and never answers another ask. The approve/deny palette commands have no card to click, so they resolve the id of the front ask only when the view has drawn that ask's card with its exact SQL (`mark_shown` / `shown_current`); before that they are unavailable. The check lives in the shared module, so T17 gets it too. |
| UI tools | `open_in_editor` and `open_results_tab` exist only in the app (a `UiHost`); they change what the user sees, never the database, and are not offered over MCP or ACP. They check that the connection is the tab's connection. |
| Re-run and open in editor | A statement in the transcript can be re-run through the normal run flow (its usual confirmation applies, the model is not called) or opened in an editor tab. The actions name the statement by a stable entry number (its position in the engine transcript counted from the start of the session: the engine's `dropped()` plus the position in `entries()`, which stays the same when the engine drops its oldest entries, see the addendum below), not by a position in the list the tab shows. The tab keeps the last 500 entries and shows a line when older ones were dropped; a number that was dropped does nothing but tell the user. |
| Cancel and close | Cancel stops the stream, the pending driver call or the wait for an approval. A call cancelled while waiting for approval is shown as "not run"; a call cancelled mid-flight is shown as **Cancelled** and means the statement may have run. It is audited as `cancelled` (T19 audits it). Closing the tab or quitting cancels, denies waiting approvals and shuts the source. Turning AI off for the connection stops its turns. |
| AI off | A connection with AI off cannot open a session: the command and the hub button explain why. |
| Transcript | Kept in memory for the life of the tab only; never written to disk. |

## Consequences

- Positive: all flows are tested end to end on SQLite without GPUI; production approval has exactly one
  implementation of the double confirmation; the approval module is reusable.
- Negative: an AI tab borrows an entry of `Workspace::queries` for its gate, so code that walks queries
  must know `uses_run_gate()`.
- A statement cancelled mid-flight is reported as possibly executed, never as "not run".
- This ADR adds a tab kind to ADR-0042 and refines ADR-0039 (UI side) and replaces nothing.

## Known limitations

- A Cancel clicked in the short window between starting a turn and the engine resetting its cancel
  flag is lost; Cancel stays enabled, so a second click works. Fixing it needs a change in
  `sqler-ai-tools`, which this task does not touch.
- The default focus of the approval card sits on a wrapper next to the Deny button, not on the button
  itself (the button owns its focus handle), so keyboard-only Deny needs one Tab. Approving always needs
  an explicit Tab and Enter or a click.

## Addendum: stable numbers over the engine cap

The engine transcript is capped (ADR-0039 addendum, 500 entries, dropped only before a `User` entry). The stable entry number is `dropped() + position in entries()`, so an entry keeps its number after older ones are dropped. The tab receives the transcript with `set_entries(all, dropped)`, keeps its own last 500, and `entry(n)` still resolves the same absolute number. A number below the first kept entry does nothing but tell the user. This supersedes "which only grows" for the engine: the transcript is no longer unbounded, and the number is not a position in the current list.
