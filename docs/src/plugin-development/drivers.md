# Drivers

A plugin can add a database driver. The decision is recorded in
[ADR-0061](../adr/0061-plugin-drivers.md). A plugin driver appears in the connection form
next to the built-in ones and is used through the same run flow, so every statement still
goes through the statement classifier and the production confirmation. A plugin driver has no
way to write around them.

Plugin drivers need a build with the `plugins` feature (off by default in the shipped binary).

## Two kinds

| | Host driver | Guest driver |
| - | - | - |
| Manifest | `kind = "host"`, `engine = "<engine-id>"` | `kind = "guest"` |
| Capability | `driver.host:<engine-id>` | `driver.guest` |
| Needs `[entry]` | no | yes |
| Who connects | the SQLer binary, with a native engine | your component |
| What your component sees | nothing | the connect parameters |

The driver id (`[[drivers]] id`) is the id stored in `connections.toml`. It is not
namespaced. A built-in driver always wins over a plugin with the same id, and when two
plugins claim one id, the first loaded wins.

## Host drivers

A host driver is a declarative shell around an engine that the binary was built with. See
the example in [Manifest](manifest.md). If the build does not include the engine, the driver
is listed as unavailable with a clear text. Host engines receive the whole connection
config; the plugin never does.

## Guest drivers

A guest driver exports the WIT `driver` interface, and optionally `driver-catalog` (world
`driver-catalog-plugin`, package `sqler:plugin@0.1.1`) for the object browser. Without
`driver-catalog` the driver works but has no introspection.

```toml
[plugin]
id = "my-db"
name = "My DB"
version = "0.1.0"
api = "0.1"
capabilities = ["driver.guest"]

[entry]
component = "my_db.wasm"

[[drivers]]
id = "my-db"
name = "My DB"
kind = "guest"
```

### What your driver receives

Only `connect(driver, connect-params)`: host, port, user, password, database and the query
pairs of the connection target as `options`. A target that is not a URL arrives as
`database`. Your driver never receives the SSH tunnel secret, TLS file paths, other
connections or the environment. It has no sockets: the only network access is HTTP under
`net.http`.

The user is told in the grant dialog that the plugin receives the host, username and
password of every connection that uses its driver. Treat the password accordingly: do not
log it and do not put it in an error message (a message that mentions the password is
replaced by a fixed text).

### Streaming

`execute` returns `rows(stream)` or `affected(n)`. The host reads the stream with
`next-batch(500)`. A batch must have at most 500 rows, each as wide as the column list, and
the whole result stays under the result byte cap; otherwise the stream fails with "the
plugin driver sent malformed rows". An empty batch ends the stream.

### Cancel, traps and sessions

There is no `cancel` export. Cancel stops the running call by dropping the plugin instance.
A cancel between batches only ends the stream. A cancel during a call, a trap or a timeout
ends every open session of the plugin; the next statement on such a session fails with
"the plugin driver session ended (the plugin was restarted, disabled or its permissions
changed)", and the user connects again. Revoking `driver.guest` or disabling the plugin has
the same effect.

## Error texts

The user sees these fixed texts, never wasmtime text.

| Situation | Text |
| - | - |
| Plugin disabled | driver `{d}` comes from plugin `{p}`, which is disabled |
| Permission not approved | driver `{d}` needs the permission `{cap}` of plugin `{p}`, which is not approved |
| Engine not in this build | driver `{d}` needs the `{e}` engine, which this build does not include |
| Id already provided | driver `{d}` is already provided by plugin `{q}` |
| Trap | the plugin driver crashed |
| Timeout | the plugin driver did not answer in time |
| Memory limit | the plugin driver ran out of memory |
| Plugin gone | the plugin driver is not available |
| Bad batch | the plugin driver sent malformed rows |
| Session over | the plugin driver session ended (the plugin was restarted, disabled or its permissions changed) |

Your own error messages reach the user after the same scrub as `query` errors.

## DuckDB

Reserved for the DuckDB host-engine plugin (P15).

## SQL Server

Reserved for the SQL Server host-engine plugin (P16).
