# sqler-driver-mssql

`SQL Server` driver implementation built on `tiberius`.

Status: implemented (P16, ADR-0054); reached through the `sqlserver` plugin as the host engine `mssql`.

Layer: L2

- Target: `sqlserver://[user[:password]@]host[:port][/database]` (`mssql://` accepted); SQL logins only.
- TLS: `rustls` with the `ring` provider; `verify-ca` and client certificates are unsupported.
- Limits: `rows_affected` is always `None`; cancel closes the connection (the session ends).
- DB tests: `sh tests/docker/start-mssql.sh <outdir>` starts a TLS-forced container (see AGENTS.md §3).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
