# ADR-0059: Plugin capability grants

- Status: accepted
- Date: 2026-10-22
- Deciders: SQLer maintainers

## Context and Problem Statement

[ADR-0056](0056-plugin-sandbox-and-capabilities.md) fixes the capability grammar, the grant key and the rule that a
plugin has nothing until the user approves it. Still open: where an approval is stored, what the approval dialog shows,
what happens to a damaged store, and how the host asks "is this approved?" without depending on the UI.

## Decision Drivers

- Default deny; an answer the user gave is remembered, and a changed plugin is asked about again.
- Manifest strings are hostile input: the dialog shows them verbatim but sanitised.
- A damaged or tampered store must never grant anything and must never be silently rewritten.
- `sqler-config` (L1) cannot depend on `sqler-plugin-api` (L1); `sqler-ui` must not depend on the host (wasmtime).

## Considered Options

- A versioned `plugin-grants.toml` of validated strings in `sqler-config`, a typed `GrantBook` in `sqler-ui` and a
  `GrantSource` trait in `sqler-plugin-api`.
- An adapter inside `sqler-plugin-host`. Rejected: the UI would depend on wasmtime.
- Per-capability approval. Deferred; the format already stores the approved list.

## Decision Outcome

### File

`<config>/plugin-grants.toml`, at the root next to `ai.toml`. It is not under `<config>/plugins/`, which is the
discovery directory. Records are sorted by plugin id on write.

```toml
version = 1
[[grant]]
key = "acme-sync@1.2.0#<64 hex>+<64 hex>"
decision = "approved"                        # or "denied"
capabilities = ["db.read", "net.http:*.example.com"]   # sorted, deduplicated; [] when denied
```

The key is the `GrantKey` display form `id@version#manifest_hash+component_hash`, verbatim. `sqler-config` checks the
shape only (at most 340 bytes, printable ASCII without spaces, a valid plugin id, 1..=300-byte capabilities, at most 32
per record); the typed parse belongs to `sqler-ui`. Unknown fields are rejected. A missing, zero or non-integer
`version` is invalid; a higher one is "newer". More than 1024 records, a duplicate plugin id or a file over 1 MiB is
invalid. On Unix the file is written with mode 0600 and a file that is group- or world-writable is invalid.

### Assumptions

- A1 Approval is all-or-nothing: Approve grants exactly the capabilities the dialog listed; Deny grants nothing. The
  approved list is still stored, so the host keeps `declared ∩ approved` and per-capability grants need no format change.
- A2 Deny, and Escape (which is Deny), is persisted as `denied`. The plugin loads with nothing granted and the plugin
  manager can reopen the dialog.
- A3 Revoke stores `denied` under the plugin's current key, so there is no prompt loop at the next start.
- A4 A broken, newer, unreadable or other-writable file locks the book: every lookup denies, Approve is disabled, and the
  file is never rewritten until the user fixes or removes it. No `.bak` is made.
- A5 The file is `<config>/plugin-grants.toml`, not under `plugins/`.
- A6 One record per plugin id. Recording a new key replaces the old record, so the file stays bounded.

### Status and re-prompt rules

For the record of the plugin id: no record means prompt. If the stored key equals the current key exactly, `denied` stays
denied and `approved` is approved (unless a stored capability no longer parses, which prompts). Any other key, so a new
version, a changed manifest or a swapped component, prompts again; the dialog then marks capabilities that were not in
the previous approval as new. Saving happens before the change takes effect: if the write fails, nothing is granted.

### Seam

`sqler-plugin-api` gains `GrantSource` (`fn approved(&self, key: &GrantKey) -> Option<CapabilitySet>`). `GrantKey` has
private fields, accessors (`id`, `version`, `hash`, `component`) and no public constructor and no `FromStr`: outside the
crate the only way to get one is `Manifest::grant_key`, which checks that the component digest fits the manifest, and a
key cannot be edited afterwards (two `compile_fail` doctests pin E0451 and E0616). Reading a stored key text back is a
separate type, `ParsedGrantKey` (`FromStr`), which compares equal to a `GrantKey` with the same text but is not accepted
by `GrantSource`; it is for comparison and display, never for lookup or enforcement. `GrantBook` in `sqler-ui`
implements the trait and returns `Some` only for an approved exact key.

### Dialog

Title "Allow plugin {name}?". One row per capability, risky ones first: `db.write`, a punycode or `localhost` host, and a
`home/...` scope are prominent. Warnings: punycode hosts are shown encoded, never decoded; wildcards say they include
every subdomain; `localhost` says it is the user's own computer; `home/Library` and `home/AppData` say they hold other
applications' data; `db.write` says every change goes through the usual confirmation. Names and descriptions are
sanitised (control and bidi characters become U+FFFD) and cut. Escape is Deny. No key chord maps to Approve, Approve is not a
tab stop, and a click on it that came from the keyboard (Enter or Space on a focused button) is ignored, so only a
pointer click approves. Commands: `plugins:approve-grant`, `plugins:deny-grant`; `plugins:approve-grant` must be reachable
only through the dialog button, not the palette or a user key binding (a requirement on the plugin manager, P7).

The dialog always shows a summary line ("N permissions, M need attention"), the grammar text of every prominent
permission in a compact block outside the scrolling area, and the full list in a scroll area with a visible scrollbar,
so a dangerous permission cannot be hidden below the fold while Approve is visible.

Accessibility trade-off (known limitation): keyboard-only users can read the dialog and Deny (Tab to Deny and Enter, or
Escape) but cannot Approve in it; a deliberate pointer click is the price of a keypress never granting. A keyboard
confirmation path (for example typing the plugin id) is not built yet and is left for the maintainer to decide.

### Consequences

- Positive: default deny survives damage; the UI and host stay decoupled; the format allows per-capability grants later.
- Negative: no per-capability choice yet. Two running app instances: every change reloads the file, changes one record
  and saves atomically, so a denial or revoke written by the other instance is not undone by an unrelated change. There
  is no file lock, so two changes in the same instant can still lose one of them (the later write wins), and an
  instance keeps answering from the grants it loaded until its next change or restart, so a revoke made by another
  instance takes effect there only then.
- Residual risk: the file is plain text, so someone with write access to the config directory can add grants. The 0600
  mode and the other-writable check reduce this; the trust level is that of `connections.toml`.
- Wildcards such as `*.co.uk` pass the grammar (no public-suffix list); the dialog shows them verbatim.

### Confirmation

Unit and property tests in `sqler-config` (parse never panics, round trip, model-based record/revoke, corrupt and newer
files stay byte-identical on save, mode checks), headless tests of `GrantBook` and the prompt text in `sqler-ui`, a
gpui test of the overlay in both themes and the `plugins-grant` screenshot scene.
