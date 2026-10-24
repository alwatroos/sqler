# sqler-driver-mssql

`SQL Server` driver implementation built on `tiberius`.

Status: implemented (P16, ADR-0054); reached through the `sqlserver` plugin as the host engine `mssql`.

Layer: L2

- Target: `sqlserver://[user[:password]@]host[:port][/database]` (`mssql://` accepted); SQL logins only.
- TLS: `rustls` with the `ring` provider; `verify-ca` and client certificates are unsupported.
- Rows affected: a text that is confidently one `INSERT`/`UPDATE`/`DELETE`/`MERGE`/`WITH` statement (no later `SET`, `USE`,
  `DECLARE`, `CREATE`, `EXEC`, `SELECT ... INTO #tmp` etc., no `;` followed by more text; not `UPDATE STATISTICS`) runs
  through `sp_executesql` and reports one count. Any other text keeps the plain batch and session state, with `None`.
  `None` also with `SET NOCOUNT ON`, with a trigger that counts, and for a batch or procedure with several counts.
  A `WITH ... SELECT` reports its row count.
- Limits: one result set per statement (later sets are dropped); `money` arrives as a float (`CAST` to `decimal(19,4)`
  for the exact value); cancel closes the connection (the session ends).
- DB tests: `sh tests/docker/start-mssql.sh <outdir>` starts a TLS-forced container (see AGENTS.md §3).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
