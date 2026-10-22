# Capabilities

A capability is a string in `[plugin] capabilities`. The user approves capabilities; the
effective set is the declared set intersected with the approved set, and the default is deny.
An ungranted host call fails with `error-code::capability-denied`; the plugin still loads.

## Grammar

ASCII, at most 300 bytes:

```
db.read | db.write | ui.panel | ai.tool | driver.guest
| net.http:<host-pattern> | fs.read:<scope> | driver.host:<engine-id>
```

## What each capability gates

| Capability | Gates |
| ---------- | ----- |
| none required | `log`, `settings`, `lifecycle`, `ui-handler.run-command` and `status`, `exporter`, `completion` (the contributions must still be declared) |
| `ui.panel` | `ui-handler.render-panel` and `panel-event`, `ui.refresh-panel` |
| `db.read` | `catalog`; `query.run` for Read-class statements |
| `db.write` | `query.run` for any other statement class |
| `ai.tool` | `ai-tools` |
| `driver.guest` | the `driver` interface |
| `net.http:<host-pattern>` | WASI HTTP outgoing requests to matching hosts |
| `fs.read:<scope>` | read-only WASI preopens of the scope |
| `driver.host:<engine-id>` | a declarative driver backed by a host engine |

Statement classification is fail-safe: an unknown statement is not Read, so it needs
`db.write`. A write goes through the same gate as in the app: production connections ask for
confirmation, and the statement is refused when there is no approver. `db.write` without
`db.read` is a manifest error.

## Host patterns (`net.http`)

Lowercase ASCII `[*.]label(.label)+[:port]`, or exactly `localhost[:port]`. A label is
`[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`; the total length is at most 253. A wildcard is only a
leading `*.` with at least two labels after it. A port is 1 to 65535 without a leading zero;
without a port, 80 and 443 match. `*.a.b` matches `x.a.b` and `y.x.a.b`, not `a.b`.

| Accepted | Rejected | Why |
| -------- | -------- | --- |
| `api.example.com` | `https://api.example.com` | scheme |
| `*.example.com` | `*`, `*.com` | bare or one-label wildcard |
| `localhost:8080` | `127.0.0.1`, `0x7f.0.0.1` | all-numeric last label (IP forms) |
| | `2130706433` | a single label |
| `example.com:8443` | `[::1]`, `example.com:0`, `example.com:080` | IP literal, bad port |
| | `user@example.com`, `example.com/path`, `example.com.` | userinfo, path, trailing dot |
| | `Example.com`, `exämple.com`, `a%2eb.com` | uppercase, non-ASCII, escapes |

Wildcards such as `*.co.uk` are accepted (there is no public-suffix list); the prompt shows
the pattern.

## File scopes (`fs.read`)

`<root>/<rel>`, or a bare `plugin` or `data`. Roots: `plugin` (the install directory), `data`
(`<config>/plugins/<id>/data`) and `home` (needs at least one segment). Segments are
`[A-Za-z0-9._-]{1,64}`, at most 16 segments and 255 bytes. A segment must not start or end with `.`.

| Accepted | Rejected | Why |
| -------- | -------- | --- |
| `plugin` | `home` | bare home |
| `data/cache` | `data/../x`, `data//x` | traversal, empty segment |
| `home/Documents/sql` | `/etc` | absolute |
| | `C:`, `data\x` | unknown root |
| | `data/.ssh`, `data/x.` | segment starts or ends with `.` |
| | `data/con`, `data/nul.txt`, `data/com1` | Windows device names |
| | `data/%2e%2e` | `%` |

## Ids

`PluginId`, engine ids and contribution ids: `[a-z][a-z0-9]*(-[a-z0-9]+)*`, 1 to 64 chars
(2 to 64 for a plugin id).

## Grant key and re-prompt

The grant key is `<id>@<version>#<hex>+<component-hex>`; each hex part is 64 lowercase hex chars. The first is the
SHA-256 of the whole parsed manifest. The second is the SHA-256 of the `.wasm` file, which the host computes when it loads
the plugin, so a replaced component under an unchanged manifest asks again. A declarative plugin (no `[entry]`) has no
component part: `<id>@<version>#<hex>`. Comments, whitespace, line endings, key order and
quoting style do not change it; array order does. Any manifest change, so any capability
change, gives a new key and the user is asked again. The prompt shows the difference from the
last approved set (added and removed capabilities). Approvals are stored per key.

## Limits

| Area | Limit |
| ---- | ----- |
| Manifest | 64 KiB, nesting depth 16, 32 capabilities, 64 entries per list, name/title/label 80 chars, tooltip 200, description 1024 bytes, input schema 16 KiB |
| Memory | 64 MiB per plugin; 256 MiB for guest drivers |
| Deadlines | `init` 5 s, render and status 1 s, completion 250 ms, command 30 s, export batch 5 s, AI tool 30 s, driver call 300 s |
| Plugin queries | default 1000 rows (cap 10 000); default timeout 30 s (cap 300 s) |
| Widget trees | 2000 nodes, depth 32, 16 KiB per string, tables of 1000 rows by 50 columns, 64 select options |
