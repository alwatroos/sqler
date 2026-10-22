# 0052 Editor AI actions

- Status: accepted
- Date: 2026-10-22

## Context and problem statement

The assist engine ([ADR-0038](0038-ai-assist-engine-prompts-and-grounding.md) is its design; the code is `sqler-ai-tools`) turns a
request into SQL, an explanation or a fix, and returns text. The workbench ([ADR-0050](0050-ai-surfaces-in-the-workbench.md))
has the AI state and the provider factory. The SQL editor now needs three actions (generate, explain, fix)
with a place for the answer, without letting generated SQL run by itself and without a new path to the
database.

## Decision drivers

- Generated SQL is never run by SQLer; running stays on the existing run path and its production gate
  ([ADR-0046](0046-keyboard-first-model-and-accessibility.md)).
- State and effects stay apart: `Workspace` is headless and returns an `Effect`; the view does the I/O
  through `SessionBridge` ([ADR-0007](0007-desktop-binary-and-ui-tokio-bridge.md)).
- One undo step per editor change; cancel must stop the provider stream ([ADR-0034](0034-ai-provider-api.md)).
- Additive `workbench.toml` values ([ADR-0045](0045-workbench-state-persistence.md)).

## Considered options

1. Explain with a plan, through a short-lived session.
2. Catalog-only: the model sees the schema through `search_schema` and `describe_table`, and explain has no plan.
3. Show every answer in a dialog.

## Decision outcome

Option 2, with the answers in the bottom panel.

| Topic | Decision |
| ----- | -------- |
| Dependency | `sqler-ui` depends on `sqler-ai-tools` (this amends ADR-0050, which allowed `sqler-ai-api` only). `sqler-ui` still has no provider crate and no HTTP. |
| Data source | The tool set reads an `EditorSource` that holds the connection info and the cached catalog. Its `acquire()` answers "not available", so the assistant never opens a session and never reads rows. Explain therefore has no plan: a plan needs a session, which costs one extra connection per request and would raise a production-gate question on `prod`. |
| Flow | Toolbar, palette, prompt bar and the "AI fix" buttons dispatch an `AiAction`. `Workspace::dispatch_assist` checks the gate and returns `Effect::RunAssist`; the view runs it on tokio and reports back with `assist_finished`. Cancel is a oneshot raced against the call; dropping the future cancels the provider stream. A late or stale answer is ignored by request id. |
| Never run | No assist path returns a run effect. Generated SQL is inserted into the editor as one undo step, and an explanation or a diff is only shown. The user runs the SQL with the normal Run, so a `prod` connection asks for confirmation as always. A test walks every returned effect. |
| Gate | An action is blocked, in this order, when: the tab is not a SQL editor; AI is off for the connection; `ai.toml` is broken; no model is set for the use case; the provider kind is not built in; a request already runs in the tab; for fix, no statement failed. The reason is the tooltip of the disabled button and the notice of the command. |
| Fix apply | A fix is applied only if the failed statement text still occurs exactly once in the current buffer; the match is replaced. Zero or several matches, or changed text, apply nothing and show "The statement changed in the editor since it failed; the fix was not applied." |
| Redaction | The engine already marks a fix with a redacted value as not applicable. For generated SQL the UI checks `contains_redaction` again and does not insert such an answer. |
| Placement | A new bottom-panel view `PanelView::Assistant` (`"assistant"`, the fourth value of `[bottom_panel] view`) that follows the active editor tab, like Results and Messages. Explain and fix reveal it. Generate uses a prompt bar under the toolbar and inserts at the cursor. |
| Commands | `ai:generate-sql` (default `mod+i`), `ai:explain-query`, `ai:fix-query`, `ai:cancel-assist`, `ai:accept-fix`, `ai:reject-fix`, and `workbench:show-assistant`. `mod+i` is free: `gpui-component` 0.8 binds neither `cmd-i` nor `ctrl-i`. |
| Audit | Every tool call of an assist request is written to `ai-audit.jsonl` with the surface `assist`. |

### Additive config values

`workbench.toml` gains one accepted value of `view` in `[bottom_panel]`: `"assistant"` (index 3 of
`BOTTOM_VIEWS`). An older SQLer reads it as an unknown value and falls back to the default view (the lenient
read of ADR-0045). The `mod+i` chord is an ordinary `DEFAULTS` row; a user override works as before.

## Consequences

- Positive: generated SQL cannot reach the database without the user's Run and the production gate; cancel
  is real; the answer has a stable place.
- Negative: explain cannot quote a plan or timings; `sqler-ui` has one more internal dependency edge.
- This ADR amends ADR-0050 (the `sqler-ui` dependency; the Assistant placement it reserved) and ADR-0045 (a
  fourth bottom-panel `view` value) additively and replaces nothing.
