# 0050 AI surfaces in the workbench

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

M5 adds AI features (natural language to SQL, explain, fix, exploration) on top of the provider API
([ADR-0034](0034-ai-provider-api.md)) and the configuration ([ADR-0035](0035-ai-configuration.md)).
Before any of them can ship, the workbench ([ADR-0042](0042-workbench-layout.md)) needs one agreed place
for the AI settings, the per-connection opt-in and the state of AI, so that the later items (assistant,
audit, session and agent views) add their views without moving anything.

## Decision drivers

- AI is off until the user turns it on per connection; the state must always be visible.
- One Settings page ([ADR-0042](0042-workbench-layout.md)), not a second settings surface.
- No I/O at startup beyond reading `ai.toml`; keychain and network only on the blocking pool or tokio
  through `SessionBridge` ([ADR-0007](0007-desktop-binary-and-ui-tokio-bridge.md)).
- Providers are compiled in by cargo feature; the UI must not depend on the provider crates.
- Additive changes to `workbench.toml` ([ADR-0045](0045-workbench-state-persistence.md)).

## Considered options

1. AI settings as a separate tab kind.
2. A section of the existing Settings page plus a side view for the live state.
3. A floating AI window.

## Decision outcome

Option 2. The owner may overrule any placement below by a follow-up ADR; each one is a single row in
one table, so moving it is cheap.

| Surface | Decision |
| ------- | -------- |
| Activity bar | A new item **AI** (Sparkles icon) in the bottom group, above Settings. `ActivityItem::BOTTOM = [Ai, Settings]`, `ActivityItem::COUNT = 5`. Clicking the active item hides the side panel, as for the other items. |
| Side view | `SideView::Ai`, stored in `workbench.toml` as `[side_panel] view = "ai"`. It shows the target connection's AI state, the configured providers and the buttons "AI settings" and "Test provider". Later items append one section each. |
| Settings | An **AI** section at the end of the Settings page: providers, models per use case, redaction, limits. The page becomes scrollable. |
| Connection dialog | An **AI** block after SSH: Off/On and, when on, "Send result rows" (Environment default, Yes, No). Editing a connection now keeps its `[connection.ai]` table (it was dropped before). |
| Status bar | An item after the database item with the Sparkles icon: "AI off", "AI not set up", "{provider} · local", "{provider} · remote" or "{provider} · local +remote". Its tooltip names the provider, kind and model and says where the schema (and result rows) go. A click shows the AI view. Hidden without a connection. |
| Commands | `workbench:show-ai` ("View: Show AI", default `mod+shift+a`; never `Mod+Shift+I`, the GPUI inspector). Module commands in `crates/sqler-ui/src/ai/action.rs`, without default chords: `ai:open-settings`, `ai:add-provider`, `ai:test-provider`, `ai:save-provider`, `ai:cancel-provider`, `ai:edit-settings`, `ai:toggle-connection`. |
| Later views | The Assistant and the Audit log open as bottom-panel views; an exploration session and an agent run open as editor tabs. This ADR only reserves the placement. |

### Additive config values

`workbench.toml` gains one accepted value of `view` in `[side_panel]`: `"ai"` (index 2 of `SIDE_VIEWS`). An older file
is read unchanged; a file written by this version with `view = "ai"` is read by an older SQLer as an
unknown value and falls back to the default view (the lenient read of ADR-0045). `keybindings.toml` needs
no change: the new default chord is an ordinary `DEFAULTS` row and a user override works as before.
`ai.toml` and `connections.toml` keep their formats.

### Provider factory

`sqler_ui::ai::AiProviderFactory` (`kinds()`, blocking `build()`) is implemented by `sqler-app` with one
`#[cfg(feature = "ai-...")]` arm per provider (`ai-ollama`, `ai-anthropic`, `ai-openai`, `ai-mistral`, all
in `default`). A kind that is not compiled in is absent from the "Add provider" buttons, and building it
returns `AiError::Unsupported`. `sqler-ui` depends on `sqler-ai-api` only. `AiStartup::load` reads
`ai.toml` once at startup; a broken file is kept as is and every AI edit is refused until it is fixed.

### Secrets

An API key lives in `Secret`, `SecretOp` and the keychain (account `<provider-id>:ai-provider-key`). It is
never written to `ai.toml`, shown in a notice, a `Debug` output or a screenshot; after a save the key
input is emptied and the editor shows "Stored in the keychain".

## Consequences

- Positive: one stable place for every later AI view; the state of AI is visible in the status bar; the
  UI crate stays free of provider crates and HTTP.
- Negative: the activity bar has one more icon; the Settings page is longer and scrolls.
- This ADR amends ADR-0042 (a fifth activity item, a third side view) and ADR-0045 (a third side panel `view`
  value) additively and replaces nothing.
