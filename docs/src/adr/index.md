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
| [0036](0036-ai-tool-schemas-guards-and-host-traits.md) | AI tool schemas, guards and host traits: one `ToolSet`, approval per statement, prod twice, no approver refuses, row and time caps | accepted |
| [0037](0037-ai-redaction-untrusted-data-and-audit-log.md) | AI redaction, untrusted-data envelope and audit log: redact by result shape, wrap every result, JSON Lines audit with rotation | accepted |
| [0038](0038-ai-assist-engine-prompts-and-grounding.md) | AI assist engine: versioned prompt templates, schema grounding through `search_schema` under a byte budget, `search_data` as a read-only `DataSearch` | accepted |
| [0040](0040-mcp-server.md) | MCP server: in-crate JSON-RPC layer (rmcp measured, not adopted), stdio and loopback HTTP with a bearer token | accepted |
| [0041](0041-acp-client.md) | ACP client: agent profiles, one supervisor task per agent, scrubbed environment, MCP descriptor and no client capabilities | accepted |
| [0042](0042-workbench-layout.md) | Workbench layout and editor groups: headless layout, groups, preview tabs, bottom panel, status bar | accepted |
| [0043](0043-visual-system-and-themes.md) | Visual system and themes: scales, embedded SQLer Dark/Light themes, env accents, contrast test | accepted |
| [0044](0044-icons.md) | Icons: `gpui-kit-assets` Lucide subset plus component assets | accepted |
| [0045](0045-workbench-state-persistence.md) | Workbench state persistence: `workbench.toml` v1, lenient read, debounced atomic writes | accepted |
| [0046](0046-keyboard-first-model-and-accessibility.md) | Keyboard-first model and accessibility: command IDs, F6 regions, labels, `query:explain` through the prod gate | accepted |
| [0047](0047-screenshot-harness.md) | Screenshot harness: headless GPUI renderer, scenes in both themes, Linux CI artifact | accepted |
| [0050](0050-ai-surfaces-in-the-workbench.md) | AI surfaces in the workbench: activity item, side view, status item, command ids, additive `side_view` value, provider factory | accepted |
| [0051](0051-app-hosted-mcp-endpoint-and-agent-hosting.md) | App-hosted MCP endpoint and agent hosting: Agent tab, endpoint and token per tab, approvals and permissions, audit surface `agent`, teardown order, cancel/close/quit bounds, agents.toml | accepted |
| [0052](0052-ai-editor-actions.md) | Editor AI actions: generate, explain and fix in the SQL editor, catalog-only source, never run, exact-match fix, Assistant panel view | accepted |
| [0053](0053-ai-session-tab-and-approvals.md) | AI session tab and approvals: pinned tab kind, lend/return turn, host channel, exact-SQL approval card, production gate reuse, UI-only tools, cancel semantics | accepted |
| [0054](0054-plugin-runtime-and-driver-feasibility.md) | Plugin runtime and driver feasibility (P1 spike): wasmtime variants and sizes, epoch interruption, DuckDB NO-GO, SQL Server NO-GO as shipped, host-provided engines as the fallback | accepted |
| [0055](0055-plugin-abi-wit.md) | Plugin ABI: the `sqler:plugin` WIT package, version rule, worlds, interfaces, WASI set | accepted |
| [0056](0056-plugin-sandbox-and-capabilities.md) | Plugin sandbox and capabilities: grammar, gates, default deny, grant key, query rule, limits | accepted |
| [0057](0057-plugin-host-runtime.md) | Plugin host runtime: two layers and features, wasmtime 50 engine configuration, epoch deadlines, limits and the three-fault rule, WASI scope (clocks and random stay linked), egress rules, extension protocol | accepted |
| [0058](0058-plugin-query-and-catalog.md) | Plugin query and catalog: `DataService`/`QueryApprover` traits, order of checks, write approval (twice on prod), limits, session lease, catalog behaviour, threat model | accepted |
| [0059](0059-plugin-capability-grants.md) | Plugin capability grants: `plugin-grants.toml`, all-or-nothing approval, re-prompt rules, damaged-file lock | accepted |
| [0060](0060-plugins-in-the-workbench.md) | Plugins in the workbench: activity item and side view, command ids, `PluginsBackend` trait, typed-id keyboard approval, grants re-read, own runtime, explicit dev paths, data folder, `plugins` feature (off by default) and the size rule | accepted |
| [0061](0061-plugin-drivers.md) | Plugin drivers: `DriverSource` overlay on the registry, host-engine vs guest drivers, what a guest sees, `driver.guest` wording, unavailable texts, WIT 0.1.1, guard parity, DuckDB licence stop | accepted |
| [0062](0062-plugin-ai-tools.md) | Plugin AI tools through the tool contract: `ExternalTools` seam in `sqler-ai-tools`, `<plugin-id>__<tool>` names, schema subset, approval through the surface's approver, per-call data and approver swap, audit scope, surfaces, no tools in `sqler-cli mcp` | accepted |
| [0063](0063-plugin-or-core-per-engine.md) | Plugin or core crate, per engine: SQL Server and `DuckDB` both stay plugins over host-provided engines (WASI guest route is NO-GO), the API gaps, what a promotion would need, `DuckDB` out of 1.0 | accepted |
| [0066](0066-update-check-and-signed-manifest.md) | Update check and signed manifest: check-only `sqler-update` client, one Ed25519-signed envelope on GitHub Releases `latest/download`, keys compiled in (none until R17), rotation, opt-in check policy, request contents, redirect policy, replay limits | accepted |
| [0067](0067-connection-import.md) | Connection import: the L2 crate `sqler-import` (pure `Source` -> `plan` -> `apply`), registry of foreign tools, duplicate and rename rules, no foreign secret values, size and path caps | accepted |
| [0068](0068-ci-pipeline-speed.md) | CI pipeline speed: what runs where, path gating with always-reporting names, caches saved on `main` only, Windows PR subset, `CI result` aggregate, rollback | accepted |
| [0069](0069-binary-size-budget-70-mb.md) | Binary size budget raised from 60 MB to 70 MB (owner decision); the plugin host stays off by default | accepted |
| [0070](0070-duckdb-engine-and-licence.md) | DuckDB native engine, sandbox and the `webpki-roots` licence question: `duckdb` 1.20000.0 has no `webpki-roots` in the graph (no exception), worker thread per session, Arrow streaming, interrupt cancel, external access and extensions off, one instance per file | accepted |
| [0071](0071-sql-server-plugin.md) | SQL Server as an in-repo plugin with a host-provided engine: plugin vs core, cancel closes the connection, ring installed beside aws-lc-rs, API gaps | accepted |
| [0072](0072-plugin-export-formats.md) | Plugin export formats: `FormatSource` overlay on the export registry, `plugin-<id>:<fmt>` ids and cleaned labels, blocking wait on the host runtime, interned strings, fixed failure texts, CLI for `plugin-` ids | accepted |

ADR number 0039 is reserved for M5.
