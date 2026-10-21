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
| [0010](0010-mysql-driver.md) | `MySQL` and `MariaDB` driver: sqlx, `KILL QUERY` cancel, no RSA password exchange | accepted |
| [0011](0011-connection-store-format.md) | Connection store format: versioned TOML, groups, derived secret references | accepted |
| [0012](0012-keychain-secrets.md) | Keychain secrets: `keyring` 4.x, reference scheme, failure policy | accepted |
| [0013](0013-ssh-tunnel-crate.md) | SSH tunnel crate: russh, strict known_hosts, loopback-only forward | accepted |
| [0014](0014-catalog-cache-and-background-refresh.md) | Catalog cache, worker concurrency and background refresh | accepted |
| [0015](0015-ddl-generation-dialects-and-quoting.md) | DDL generation: dialects and quoting | accepted |
| [0016](0016-connect-pipeline.md) | Connect pipeline: `Connector` trait in `sqler-ui`, pipeline in `sqler-app` | accepted |
| [0017](0017-object-browser-sessions-and-drop-confirmation.md) | Object browser: browser session per connection, short-lived action sessions, DROP confirmation | accepted |
| [0018](0018-rsa-alternatives.md) | RSA without the `rsa` crate: a `ring` SSH signer and a vendored `sqlx-mysql` patch on `aws-lc-rs` | accepted |
| [0019](0019-tls-server-name-through-tunnel.md) | TLS server name through a tunnel: `TlsConfig::server_name`, local Unix-socket bridge | accepted |
| [0020](0020-sql-text-services.md) | SQL text services: own lexer, fail-safe classification | accepted |
| [0021](0021-query-history-and-saved-queries.md) | Query history and saved queries: JSON Lines history, versioned TOML saved queries | accepted |
| [0022](0022-catalog-snapshot-name-index.md) | Catalog snapshot, name index and on-demand column loading | accepted |
| [0023](0023-completion-engine.md) | Completion engine: own lexer, schema source trait, ranking | accepted |
| [0024](0024-query-execution.md) | Query execution: headless pipeline in `sqler-ui`, bound parameters, per-statement results, prod gate | accepted |
| [0025](0025-sql-editor-component.md) | SQL editor component: `gpui-component` editor with headless logic | accepted |
| [0026](0026-editor-intellisense.md) | Editor intellisense: completion, hover and diagnostics in the SQL editor | accepted |
| [0027](0027-bounded-result-model.md) | Bounded result model: credit gate, fetch more and compact rows | accepted |
| [0028](0028-export-writer-api.md) | Export writer API: push-style row writers, a format registry and SQL literals | accepted |
| [0029](0029-parquet-and-xlsx-writers.md) | Parquet and XLSX writers: own streaming zip, low-level Parquet API, two cargo features | accepted |
| [0030](0030-staged-edits-and-dml.md) | Staged edits and DML: bound statements, single-table editability, one-transaction apply | accepted |
| [0031](0031-client-side-sort-and-filter.md) | Client-side sort and filter: published display order, resumable job on the UI thread | accepted |
| [0032](0032-grid-export.md) | Grid export: loaded rows or a fresh run of a read statement, writer on the blocking pool, hidden partial file, Parquet in the app default | accepted |
| [0033](0033-inline-editing-ui.md) | Inline editing: apply session, transaction flag, schema probe, in-place overlay and a single gate | accepted |
| [0034](0034-ai-provider-api.md) | AI provider API: boxed futures, crate-owned event stream, cancel by drop, increment usage | accepted |
| [0035](0035-ai-configuration.md) | AI configuration: `ai.toml`, provider key references, per-connection opt-in, connections format v2 | accepted |
| [0041](0041-acp-client.md) | ACP client: agent profiles, one supervisor task per agent, scrubbed environment, MCP descriptor and no client capabilities | accepted |
| [0042](0042-workbench-layout.md) | Workbench layout and editor groups: headless layout, groups, preview tabs, bottom panel, status bar | accepted |
| [0043](0043-visual-system-and-themes.md) | Visual system and themes: scales, embedded SQLer Dark/Light themes, env accents, contrast test | accepted |
| [0044](0044-icons.md) | Icons: `gpui-kit-assets` Lucide subset plus component assets | accepted |
| [0045](0045-workbench-state-persistence.md) | Workbench state persistence: `workbench.toml` v1, lenient read, debounced atomic writes | accepted |
| [0046](0046-keyboard-first-model-and-accessibility.md) | Keyboard-first model and accessibility: command IDs, F6 regions, labels, `query:explain` through the prod gate | accepted |
| [0047](0047-screenshot-harness.md) | Screenshot harness: headless GPUI renderer, scenes in both themes, Linux CI artifact | accepted |

ADR numbers 0036 to 0040 are reserved for M5.
