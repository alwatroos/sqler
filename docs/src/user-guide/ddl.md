# DDL and scripts

For an object in the object browser SQLer generates four scripts and a "copy name". They are built for the dialect of the connection: `PostgreSQL`, `MySQL`/`MariaDB` or `SQLite`. Generating a script runs nothing against the server. Each script opens in a new [SQL editor](sql-editor.md) tab on the object's connection, ready to edit; you run it yourself.

## Quoting

Every identifier is quoted, whatever it is called. `PostgreSQL` and `SQLite` use double quotes, `MySQL` uses backticks, and a quote character inside a name is doubled. Reserved words, mixed case, spaces and quotes therefore need no special handling: `order"s` becomes `"order""s"`. A name that is empty or contains a NUL character is rejected with an error.

The text of a server definition, a column default and a native column type are copied as the server reports them, without escaping.

## Copy name

The fully qualified, fully quoted name. A schema-level object (table, view, function, index and so on) is `"schema"."name"`; on `MySQL` the database takes the place of the schema. Without a qualifier it is the bare quoted name. Databases, roles, schemas and extensions are the quoted bare name.

## Supported scripts

`CREATE` and `DROP` support the same kinds:

| Dialect | Kinds |
| ------- | ----- |
| `PostgreSQL` | all 13 kinds |
| `MySQL`/`MariaDB` | database, schema, table, view, function, procedure, trigger, index, sequence |
| `SQLite` | table, view, trigger, index |

`SELECT *` is available for tables and views, and for materialized views on `PostgreSQL`. The `INSERT` template is available for tables. Any other combination is reported as unsupported.

## CREATE

- Table: built from the model, not from server text. It holds the columns with their type, `DEFAULT` and `NOT NULL`, the primary key, and the foreign keys with `ON UPDATE` and `ON DELETE` actions (omitted when `NO ACTION`). Constraint names are kept when known. Index definitions fetched from the server follow the table as separate statements. If a column has no native type, a per-dialect fallback type is used (`SQLite` leaves the column without a type for unknown types). `SQLite` foreign keys name the referenced table without a schema, because its grammar has no place for one. A table without columns gives `CREATE TABLE "name" ();` on `PostgreSQL`; `MySQL` and `SQLite` cannot have such a table, so the script is an error.
- View, materialized view, function, procedure, trigger, index, sequence, type: the server's definition text, ending in `;`. A view body that does not start with `CREATE` is wrapped in `CREATE VIEW ... AS` or `CREATE MATERIALIZED VIEW ... AS`.
- Database, schema, extension, role: `CREATE DATABASE|SCHEMA|EXTENSION|ROLE "name";` when the server has no text. On `MySQL` a schema is `CREATE DATABASE`.
- Any other kind without definition text is an error.

`MySQL` routine definitions contain inner `;`. The editor splits a script the way the server reads it (see [Running queries](running-queries.md)), so a `BEGIN ... END` body stays in one statement.

## SELECT *

```sql
SELECT *
FROM "public"."users"
LIMIT 1000;
```

All three dialects use `LIMIT`. The default limit is 1000.

## INSERT template

Lists every column. `PostgreSQL` uses numbered placeholders (`$1, $2`), `MySQL` and `SQLite` use `?`. A table without columns gives `INSERT INTO ... DEFAULT VALUES;` on `PostgreSQL` and `SQLite`, and `INSERT INTO ... () VALUES ();` on `MySQL`.

## DROP

`DROP <KIND> <name>;`, with the keyword of the object kind. Three forms differ:

- `PostgreSQL` trigger: `DROP TRIGGER "name" ON "schema"."table";`
- `MySQL` index: `` DROP INDEX `name` ON `db`.`table`; ``
- `MySQL` schema and database: `DROP DATABASE`

The trigger and index forms need the parent table; without it the script is an error. The script is a plain `DROP`, without `IF EXISTS` or `CASCADE`. `PostgreSQL` `DROP FUNCTION` has no argument list, so it fails for overloaded functions.

A `DROP` never runs without confirmation; the confirmation is part of running scripts from the browser (T10).

See [ADR-0015](../adr/0015-ddl-generation-dialects-and-quoting.md).
