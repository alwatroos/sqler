# ADR-0076: AI chat panel

- Status: accepted
- Date: 2026-10-26
- Deciders: SQLer maintainers

## Context and Problem Statement

The AI session tab ([ADR-0053](0053-ai-session-tab-and-approvals.md)) is a full editor tab with approvals, a
results hand-off and re-run. Operators also want a light assistant beside the editor: ask about the schema or the
data while writing SQL, without leaving the editor and without any chance of a change to the database. The chat must
sample data and analyse it under hard bounds.

## Decision Drivers

- The chat must never change data, whatever the model asks for and whatever the connection's environment is.
- Reuse the session engine ([ADR-0039](0039-ai-exploration-session-engine.md), ADR-0053): turns, the host channel, cancel and
  close-on-quit are keyed by `TabId` and must keep working unchanged.
- The panel sits on the right, next to the editor, and does not replace the object browser on the left.
- Rows shown to the model stay inside the AI caps of `ai.toml`.

## Considered Options

- A new side view (`SideView::Chat`): rejected, it replaces the object browser on the left, while the brief asks for
  the right side next to the editor.
- Another AI session tab in the strip: rejected, a tab takes the editor's place and carries approvals and results
  tabs that the chat must not have.
- A right-side panel that holds one `AiSessionTab` under a reserved id: chosen.

## Decision Outcome

### Placement and state

The panel is a third, fixed-width child at the right end of the `side-split` row; the editor child stays the flexible
one. Visibility and width live in `WorkbenchLayout` (`chat_visible`, `chat_width`; actions `ToggleChatPanel` and
`SetChatWidth`, command `workbench:set-chat-width`). Width is clamped to 280..=900 px (default 380). The panel is a
region of the F6 walk, between the bottom panel and the status bar. It opens with the command `ai:toggle-chat` ("AI:
Toggle Chat Panel") or the right-side "Chat" item of the status bar. There is no default chord.

The conversation is one `AiSessionTab` stored in `Workspace::ai_sessions` under the reserved `TabId::CHAT_PANEL`
(`u64::MAX - 1`, the precedent of `TabId::PANEL_HISTORY`); it never has a strip tab. Hiding the panel keeps the
conversation in memory; `ai:new-chat` discards it. There is one chat at a time, bound to the connection it was started
on. When the active connection differs, the panel offers a new chat instead of silently switching.

### The read-only rule

A new `SessionMode::ChatPanel` on `AiSessionStart` builds a stricter session than a tab:

- The `ConnectionInfo` is built with `read_only = true`. The guard therefore returns `ToolError::ReadOnly` for every
  non-read statement before any approval is asked.
- There is no approver. Approval-and-run never exists in the chat, so it cannot be reached by a model, a bug or a
  prompt injection. If an ask ever reached the workspace for `CHAT_PANEL`, it is denied (defence in depth).
- The production gate is kept only through the editor: SQL the chat produced is copied or opened in an editor tab and
  runs there under the normal run flow and its confirmations. The chat never runs it and never adds it to the re-run
  list.
- The `read_only` error text says "the connection is read-only" even when the connection is writable. The panel maps
  the codes `read_only` and `no_approver` to its own text ("refused (read-only chat)") and shows a note that
  statements that change data are refused here.

### Caps and tools

- Limits are forced capped: `unlimited = false` whatever `ai.toml` says, with `max_rows` and `timeout_secs` from
  `[limits]`. The header shows `caps_label`: "Read-only · up to N rows · T s per query".
- There is no `UiHost`, so `open_results_tab` and `open_in_editor` do not exist: a results tab would show rows outside
  the AI caps. An UI call for `CHAT_PANEL` is answered with the wrong-connection host error (defence in depth).
- `search_data` is attached through `AssistSearch`, so the model can search values under the same caps and redaction.
- Prompt: the chat has its own system prompt (`chat_system_prompt`, `PromptKind::Chat`, `CHAT_PROMPT_VERSION` 1); the
  tab prompt (`SESSION_PROMPT_VERSION` 1) is unchanged. It states that only reads run and there is no approval (writes
  are offered as SQL for the user), steers analysis to aggregate queries in the database, with `sample_rows` to learn
  the shape of the data, names the caps and says a capped result is a sample, not the table, and asks for a closing
  summary with the SQL used. The untrusted-data rule is kept.
- Drivers without a SQL dialect (for example DuckDB, or a plugin that declares none) classify every statement as
  unknown, so the read-only chat refuses all queries there and `sample_rows` is unsupported: schema questions only.
- Redaction ([ADR-0037](0037-ai-redaction-untrusted-data-and-audit-log.md) (redaction)) applies as for every surface.

### Loop, limits and audit

The tool loop and session limits are the ADR-0039 defaults. Calls are audited with a new surface, `Surface::Chat`
(wire value `chat`, label "Chat panel" in the audit panel and its filter). An older SQLer reading a newer
`audit.jsonl` skips the lines with `surface: "chat"` (the reader is lenient).

### Generated SQL

Statements that ran, and fenced SQL blocks in answers (info string empty or `sql`, at most 8 per answer), can be
copied (`ai:copy-chat-sql`) or opened in an editor tab (`ai:open-chat-sql`). They are never run by the chat.

### Persistence

`workbench.toml` gains an additive `chat_panel` table (`visible`, `size`, `view = "chat"`), read as leniently as the
other panels (this amends [ADR-0045](0045-workbench-state-persistence.md)). `WORKBENCH_VERSION` stays 1. A file
without the table loads with the default (hidden, 380 px); an older SQLer ignores the table. Conversations are never
persisted.

## Consequences

- One more column can squeeze the editor on small windows; the editor keeps the flexible size and the chat is
  resizable (280 px minimum).
- The chat cannot change data even by approval. Users who want a write take the SQL to an editor.
- Six AI commands and one workbench command are added. No dependency is added.
