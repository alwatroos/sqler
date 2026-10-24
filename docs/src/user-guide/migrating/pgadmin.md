# Migrating from pgAdmin

SQLer reads the server list of pgAdmin from a `servers.json` file. pgAdmin does not keep that file
for you, so you export it first.

## Where pgAdmin keeps its connections

SQLer does not read pgAdmin's own storage. It reads a `servers.json` file, which pgAdmin writes only
when you ask for it.

## Export `servers.json` first

1. In pgAdmin, export the server list to a JSON file.
2. Save the file as `servers.json`. SQLer finds it without help in these places:
   - `~/.pgadmin/servers.json` (macOS, Linux)
   - `%APPDATA%\pgAdmin\servers.json` (Windows)
   - `~/Downloads/servers.json`
3. If you saved it somewhere else, you can still use it: choose it yourself in the wizard with
   **Choose file...**.

The file does not hold the passwords pgAdmin keeps. You type them in the Passwords step.

## Import with the wizard

The steps are the same for every tool; see
[Import from other tools](../connections.md#import-from-other-tools) for the full walkthrough.

1. Open the wizard with **Import...** in the New connection form, or run **Import Connections**
   (`connection:import`).
2. On the Source step (the "Import connections" dialog), select the **pgAdmin** row. If your file is
   in one of the places above, the row shows it; otherwise it says "Not found in the usual places.
   Choose the file." Use **Choose file...** and press **Next**.
3. On the Preview step ("Choose what to import"), check the badges (add, rename, duplicate, skipped) and clear the entries you do not
   want. A skipped entry shows its reason.
4. On **Passwords**, type the database password, the SSH password or the SSH key passphrase of each
   connection that needs one, or leave the field empty.
5. On the Summary step ("Import finished"), read the result. Escape cancels at any step and writes nothing.

![The Source step](../../screenshots/after/import-source-dark.png)

## What is and is not imported

Imported:

- Name, host, port and the maintenance database (`postgres` when it is empty). The user is kept;
  the password is not.
- The group. A pgAdmin server group becomes one SQLer connection group.
- TLS: the SSL mode, and the paths of the root certificate, client certificate and client key. The
  mode `allow` becomes `prefer` (with a note). The client certificate and key must come as a pair, or
  both are dropped (with a note). The files are not opened.
- The colour, when it is written as `#rrggbb`.
- An SSH tunnel: host, port (22 when empty), user, and password or key file as the way to sign in.

Passwords offered in the Passwords step: the database password (not offered when the entry has no
user or uses Kerberos), and the SSH password or SSH key passphrase when a tunnel is used.

Not imported:

- Every other field of the entry is listed by name as unsupported in the Preview step, so you see what
  you need to set again by hand. The same holds for connection parameters beyond the SSL ones.
- Entries that name a `pg_service` instead of a host are skipped: "pg_service entries are not
  supported".
- Entries with a missing or invalid host, port, SSH host, SSH user or SSH port are skipped with a
  short reason, for example "the host is missing" or "the port is not valid".
- Entries that SQLer already has are shown as "already saved as connection ..." and the same server
  twice in one file as "same server as entry N of this file".

The importer follows the format of the `servers.json` file. If a file does not import, report it.

## Feature map

| In pgAdmin | In SQLer | Where |
| ---------- | -------- | ----- |
| Object tree: schemas, tables, views, functions, triggers | `workbench:show-connections` | [Object browser](../object-browser.md) |
| Roles and extensions in the tree | `workbench:show-connections` | [PostgreSQL](../databases/postgresql.md) |
| Open the rows of a table | `object:open-data` | [Object browser](../object-browser.md) |
| DDL for an object | `object:generate-create` | [DDL and scripts](../ddl.md) |
| Properties of an object | `object:properties` | [Object browser](../object-browser.md) |
| Query tool | `editor:new` | [SQL editor](../sql-editor.md) |
| Run a statement | `query:run-statement` | [Running queries](../running-queries.md) |
| EXPLAIN visualiser | Not yet. `query:explain` shows the plan of the statement. | [Running queries](../running-queries.md#explain) |
| Query history | `history:open` | [Query history](../query-history.md) |
| Edit data in a grid | `grid:edit-cell` | [Results grid](../results-grid.md) |
| Export results to a file | `grid:export` | [Results grid](../results-grid.md#export) |
| Server groups | `connection-group:new` | [Connections](../connections.md) |
| Servers over an SSH tunnel | `connection:new` | [SSH tunnels](../ssh-tunnels.md) |
| Dashboard with server activity | Not yet | |
| Backup and restore | Not yet | |
