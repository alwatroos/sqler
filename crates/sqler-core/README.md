# sqler-core

Domain types for `SQLer`: connection, catalog, query, result set, value and data type. Pure: no I/O, no async, no UI.

Status: API v0 (M0)

Modules: `error` (`CoreError`), `connection` (`DriverId`, `ConnectionConfig`), `query` (`Query`),
`value` (`Value`, `DataType`), `result` (`Column`, `Row`, `ResultSet`), `catalog` (`Catalog`, `Schema`,
`Table`, `TableKind`). All types are re-exported at the crate root.

Layer: L0

See [AGENTS.md](../../AGENTS.md) for the project handbook.
