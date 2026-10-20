# Architecture decisions

An Architecture Decision Record (ADR) captures one significant decision, its context and its
consequences. SQLer uses the MADR format.

- ADRs are never deleted. A changed decision is recorded by a new ADR that supersedes the old one.
- Files are named `NNNN-title.md`, with the next free four-digit number.
- The full process is in [ADR-0000](0000-adr-process.md).

| ADR | Title | Status |
| --- | ----- | ------ |
| [0000](0000-adr-process.md) | ADR process | accepted |
| [0001](0001-ui-framework-spike-plan.md) | UI framework spike plan | accepted |
| [0002](0002-plugin-abi-wasm-component-model.md) | Plugin ABI: WASM Component Model | accepted |
| [0003](0003-ai-tool-contract.md) | AI tool contract | accepted |
| [0004](0004-driver-api.md) | Driver API: boxed futures, event stream, session cancellation | accepted |
| [0005](0005-config-files-and-keybindings.md) | Config files and keybindings: TOML files, chord grammar, std-only directory lookup | accepted |
| [0006](0006-grid-data-model-and-bench-harness.md) | Grid data model and bench harness | accepted |
| [0007](0007-desktop-binary-and-ui-tokio-bridge.md) | Desktop binary and UI-thread/tokio bridge | accepted |
| [0008](0008-object-model-lazy-introspection-value-types-tls.md) | Object model, lazy introspection, typed values and TLS options | accepted |
| [0009](0009-postgres-driver-tls-db-tests.md) | `PostgreSQL` driver: sqlx, server-side cancel, rustls TLS, DB test convention | accepted |
| [0011](0011-connection-store-format.md) | Connection store format: versioned TOML, groups, derived secret references | accepted |
| [0012](0012-keychain-secrets.md) | Keychain secrets: `keyring` 4.x, reference scheme, failure policy | accepted |
| [0013](0013-ssh-tunnel-crate.md) | SSH tunnel crate: russh, strict known_hosts, loopback-only forward | accepted |
| [0014](0014-catalog-cache-and-background-refresh.md) | Catalog cache, worker concurrency and background refresh | accepted |
| [0015](0015-ddl-generation-dialects-and-quoting.md) | DDL generation: dialects and quoting | accepted |
| [0016](0016-connect-pipeline.md) | Connect pipeline: `Connector` trait in `sqler-ui`, pipeline in `sqler-app` | accepted |
