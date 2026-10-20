# sqler-core

Domain types for `SQLer`: connection, object model, query, result set, value and data type. Pure: no I/O, no async, no UI.

Status: API v1 (M2)

Modules: `error` (`CoreError`), `connection` (`DriverId`, `ConnectionConfig`), `query` (`Query`),
`value` (`Value`, `DataType`), `result` (`Column`, `Row`, `ResultSet`), `catalog` (object model: `ObjectKind`, `ObjectPath`, `DbObject`, `Listing`,
`Table`, `ColumnInfo`, `PrimaryKey`, `ForeignKey`). `connection` also has `TlsMode` and `TlsConfig`; `value` also has `Date`, `Time`, `Timestamp`, `TimestampTz` and `Decimal`. All types are re-exported at the crate root.

Layer: L0

See [AGENTS.md](../../AGENTS.md) for the project handbook.
