# SQLite

The `SQLite` driver (id `sqlite`) uses a bundled `SQLite`, so no system library is needed.

## Targets

| Target | Meaning |
| ------ | ------- |
| `:memory:` | A private in-memory database, gone when the session closes. |
| a file path | The database file. |
| `sqlite:` URL | A `sqlite:` URL, for example `sqlite:data/app.db`. |

## Object browsing

`SQLite` has no database level and no server roles. Its schemas are `main`, `temp` and every
database added with `attach`. The driver supports these kinds:

| Kind | Notes |
| ---- | ----- |
| Schema | `main`, `temp` and attached databases, in the order `SQLite` reports them. |
| Table | Columns, nullability, defaults, primary key and foreign keys. |
| View | Columns, as for a table. |
| Index | All indexes of a schema, or those of one table. |
| Trigger | All triggers of a schema, or those of one table. |

All other kinds (database, materialized view, function, procedure, sequence, type, extension, role)
are unsupported and listed as such, without an error. Internal objects whose names start with
`sqlite_` (such as `sqlite_sequence` and automatic indexes) are not listed. Objects are ordered by
name.

Column types map from the declared type, ignoring case and any `(...)` suffix. The names `bool`,
`boolean`, `date`, `time`, `datetime`, `timestamp`, `json`, `jsonb`, `decimal` and `numeric` map to
their own types. Other names follow the `SQLite` affinity rules: a name containing `int` is an
integer, `char`, `clob` or `text` is text, `blob` is binary, `real`, `floa` or `doub` is a float, and
anything else is other. Foreign keys with no referenced column refer to the primary key of the
referenced table.

## Definition text

The `CREATE` statement of a table, view, index or trigger is read from `sqlite_master`, exactly as
`SQLite` stored it. Automatic indexes have no text.

## Dates and times

`SQLite` stores dates as text, real or integer values. The driver decodes only text, and only by the
declared type of the column:

| Declared type | Text is decoded as |
| ------------- | ------------------ |
| `DATE` | a date |
| `TIME` | a time |
| `DATETIME`, `TIMESTAMP` | a timestamp, or a timestamp with zone if the text ends in `Z` or an offset |

The separator may be a space or `T`, and values print in the normalised form, so
`2024-01-02T03:04:05` shows as `2024-01-02 03:04:05`. Limits:

- Text that does not parse stays text, and integer or real cells stay numbers, even in a date column.
- The driver reads the declared type of a column only for plain table columns. `sqlx` does not report
  `JSON` and `DECIMAL` declared types, so those columns show as text or numbers.
- Expressions such as `date('now')` have no declared type and give text.

Date, time, decimal and JSON values used as query parameters are bound as their display text. A decimal bound into a `NUMERIC` or `DECIMAL` column is converted
to a real number by the column affinity, so it reads back as a float and its scale is lost (`1.50`
reads back as `1.5`).

## TLS

`SQLite` is a local file, so TLS settings do not apply.
