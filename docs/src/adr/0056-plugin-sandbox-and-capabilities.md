# ADR-0056: Plugin sandbox and capabilities

- Status: accepted
- Date: 2026-10-22
- Deciders: SQLer maintainers

## Context and Problem Statement

[ADR-0002](0002-plugin-abi-wasm-component-model.md) names the capabilities and says they are approved by the user.
[ADR-0055](0055-plugin-abi-wit.md) fixes the ABI. Still open: the exact grammar, what each host call requires, how a
grant is keyed and when it is asked again, how plugin queries meet the production gate, and the resource limits.

## Decision Drivers

- No ambient authority; the default is deny.
- Strings from a manifest are hostile input: the grammar is exact, ASCII and bounded.
- A denied plugin still loads ([ADR-0002](0002-plugin-abi-wasm-component-model.md) Confirmation).
- Plugin writes never bypass the app's confirmation flow
  ([ADR-0024](0024-query-execution.md), [ADR-0036](0036-ai-tool-schemas-guards-and-host-traits.md)).

## Considered Options

- An exact capability grammar, a gate table in the host, a grant key over the whole manifest.
- Free-form permission strings with a prompt per call.
- A grant key over the capability list only.

## Decision Outcome

### Grammar

ASCII, at most 300 bytes, exactly one of:

`db.read | db.write | ui.panel | ai.tool | driver.guest | net.http:<host-pattern> | fs.read:<scope> | driver.host:<engine-id>`

Anything else is an error (`CapabilityError`). A kind that takes an argument without one, or one that takes none with
one, is an error. `Display` and `FromStr` round-trip.

**Host pattern.** Lowercase ASCII `[*.]label(.label)+[:port]`, or exactly `localhost[:port]`. A label is
`[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`; the total length is at most 253. A wildcard is allowed only as a leading `*.` with
at least two labels after it. The port is 1..=65535 without a leading zero; an absent port means 80 and 443.

| accepted | rejected | why |
| -------- | -------- | --- |
| `api.example.com` | `Example.com` | uppercase |
| `*.example.com` | `*`, `*.com` | bare wildcard, one label after it |
| `example.com:8443` | `example.com:0`, `example.com:08`, `example.com:65536` | bad port |
| `localhost`, `localhost:5432` | `127.0.0.1`, `127.1`, `0x7f.0.0.1` | an all-numeric last label blocks IP forms |
| | `2130706433` | a single label |
| `a-b.example.org` | `[::1]`, `a.b:c:d` | brackets, a second `:` |
| | `https://example.com`, `user@example.com`, `example.com/x`, `example.com?x`, `example.com#x`, `a%2eb.com` | scheme, userinfo, path, query, fragment, percent |
| | `example.com.`, `exämple.com`, `a b.com` | trailing dot, non-ASCII, whitespace |

`*.a.b` matches `x.a.b` and `y.x.a.b`, not `a.b`. Matching compares the host in lowercase. There is no public-suffix
list, so `*.co.uk` is accepted; the prompt shows the pattern. Resolving a declared host to a private address (DNS
rebinding, `nip.io`-style names) is checked by the host on the resolved address at connect time (P4).

Notes for P4 and P6: the prompt shows a punycode host as punycode (`xn--`), never decoded; P4 strips a single trailing
dot from the URL host before matching; a broad `home/...` scope is gated only by the prompt.

**Fs scope.** `<root>/<rel>`, or a bare `plugin` or `data`. Roots: `plugin` (the install directory), `data`
(`<config>/plugins/<id>/data`), `home` (needs at least one segment). Segments are `[A-Za-z0-9._-]{1,64}`; at most 16
segments and 255 bytes. Rejected: `.`, `..`, an empty segment, a segment starting with `.`, `\`, `:`, a leading `/`, NUL,
`%`, and Windows device names (`con`, `nul`, `aux`, `prn`, `com1` to `com9`, `lpt1` to `lpt9`, any case, with or without an
extension). Examples: accepted `plugin`, `data`, `data/cache`, `home/Documents/sql`; rejected `home`, `data/../x`, `/etc`,
`data/.ssh`, `C:/x`, `data\x`, `data/%2e%2e`, `data/con.txt`, `data//x`. A scope resolves by joining segments with
`Path::join`. The host canonicalizes and refuses a symlink that leaves the scope when it preopens (P4).

**Ids.** `PluginId`, `EngineId` and `ContributionId`: `[a-z][a-z0-9]*(-[a-z0-9]+)*`, length 1..=64 (2..=64 for
`PluginId`). Tool names: `[a-z][a-z0-9_]{0,47}`. Setting keys: `[a-z][a-z0-9_.-]{0,63}`.

### Gates

The host gates every call by interface:

| interface | gate |
| --------- | ---- |
| `log`, `settings`, `lifecycle`, `ui-handler` `run-command` and `status`, `exporter`, `completion` | none (the contribution must be declared in the manifest) |
| `ui-handler` `render-panel` and `panel-event`, `ui` `refresh-panel` | `ui.panel` |
| `catalog` | `db.read` |
| `ai-tools` | `ai.tool` |
| `driver` | `driver.guest` |
| WASI http | `net.http` (a matching host) |
| WASI filesystem | `fs.read` (a matching scope; read-only) |
| `query` | the query rule below |

**Query rule.** The statement is classified with the fail-safe `sqler_sql` classifier (Unknown is not Read). A Read-class
statement needs `db.read`. Any other class needs `db.write` and then goes through the same gate as the app: the
[ADR-0024](0024-query-execution.md) production confirmation, and a refusal when there is no approver, as in
[ADR-0036](0036-ai-tool-schemas-guards-and-host-traits.md). P5 may tighten this but never loosen it. A manifest with
`db.write` and without `db.read` is an error.

**Default deny.** The effective set is `declared ∩ approved`. A call without its capability returns
`error-code::capability-denied`; the plugin still loads.

### Grant key and re-prompt

`ManifestHash` is SHA-256 of `b"sqler-manifest-v1\0"` followed by an encoding of the parsed document: a string is `s`,
a u64 big-endian length and the bytes; an integer is `i` and an i64 big-endian; a float is `f` and the u64 bits (NaN as
`0x7ff8000000000000`); a bool is `b` and 0 or 1; a datetime is `d` and its TOML text as a string; an array is `a`, a
u64 count and the items in order; a table is `m`, a u64 count and the entries sorted by key bytes. Comments,
whitespace, CRLF, quoting style, key order and `1_000` against `1000` do not change the hash; array order does (a
re-prompt, the safe side). It prints as 64 lowercase hex characters.

`ComponentDigest` is the SHA-256 of the `.wasm` bytes. P6 computes it at load time, from the bytes it is about to
instantiate, and puts it in the key. `GrantKey` is `{ id, version, hash, component: Option<ComponentDigest> }` and
prints as `<id>@<version>#<hash-hex>+<component-hex>`; a declarative plugin (no `[entry]`) has `component: None` and
prints as `<id>@<version>#<hash-hex>`. A component swapped under an unchanged manifest therefore gives a new key and
asks again. `Manifest::grant_key_with_component` builds the key of a plugin with an `[entry]` (and returns `None`
without one). The user's approval is stored under that key. Any manifest change, so any
capability change, gives a new key and asks again; `CapabilitySet::diff` shows what was added and removed. The prompt
appears at first load of a plugin (and again after a change), listing the capabilities and the declared
contributions. Persistence is P6.

### Limits

Policy values (`Limits::DEFAULT`), read by the host:

| area | value |
| ---- | ----- |
| manifest | 64 KiB; nesting depth 16; 32 capabilities; 64 entries per contribution list; name, title and label 80 chars; tooltip 200; description 1024 bytes; input schema 16 KiB |
| memory | 64 MiB per instance (256 MiB for a guest driver) |
| time | epoch tick 10 ms; deadlines: `init` 5 s, `render` and `status` 1 s, `completion` 250 ms, command 30 s, export batch 5 s, AI tool 30 s, driver call 300 s |
| query | `max-rows` default 1000, cap 10 000; timeout default 30 s, cap 300 s |
| widgets | 2000 nodes; depth 32; 16 KiB per string; tables up to 1000 rows by 50 columns; 64 select options |

### Consequences

- A manifest is validated before any code runs; the first error wins and names a dotted field path.
- A panel needs `ui.panel`, an AI tool needs `ai.tool`, a host driver needs `driver.host:<its engine>`, a guest driver
  needs `driver.guest`; everything except host drivers and settings needs an `[entry]`.
- Command, status, menu, exporter and completion contributions need no capability; they are in the manifest, so the
  grant key covers them and the prompt shows them.
- Plugin writes go at least through the ADR-0024 gate; stricter confirmation on non-prod environments is left to P5.

### Confirmation

Table and property tests in `sqler-plugin-api` (grammar, hostile hosts and scopes, hash stability, grant-key round
trip), and the host tests of P4 for runtime denial.

## Pros and Cons of the Options

### Exact grammar, gate table, whole-manifest key (chosen)

- Good: small attack surface, any change re-prompts, deterministic.
- Bad: harmless manifest edits (for example a title) re-prompt too, and so does every rebuild of the component.

### Free-form permission strings

- Bad: unbounded parsing surface, no way to review them.

### Key over the capability list only

- Bad: a contribution change (a new panel or tool) would pass unseen.

## More Information

- [ADR-0002](0002-plugin-abi-wasm-component-model.md), [ADR-0024](0024-query-execution.md),
  [ADR-0036](0036-ai-tool-schemas-guards-and-host-traits.md), [ADR-0055](0055-plugin-abi-wit.md)
