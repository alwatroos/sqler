# User guide

SQLer 1.0 connects to: SQLite, PostgreSQL, MySQL/MariaDB, SQL Server. This guide covers every part of the desktop app and the command-line tool.

## Get started

- [Install](install.md): download, verify and start SQLer
- [Workspace](workspace.md): the window with the activity bar, side panel, editor groups, bottom panel and status bar
- [Command palette](command-palette.md): run any command by name
- [Keybindings](keybindings.md): default shortcuts and how to rebind them
- [Settings](settings.md): the config directory, `settings.toml`, the Settings tab and update checks
- [Themes](themes.md): light, dark and system appearance, the UI font size and the environment colours

## Connect

- [Connections](connections.md): the connection manager, groups, TLS and SSH settings, import from other tools, the `connections.toml` format
- [Secrets](secrets.md): passwords and passphrases in the OS keychain
- [SSH tunnels](ssh-tunnels.md): connecting through a bastion host
- [Databases](databases/index.md): object kinds, value display and TLS modes per database
  - [SQLite](databases/sqlite.md)
  - [PostgreSQL](databases/postgresql.md)
  - [MySQL and MariaDB](databases/mysql.md)
  - [SQL Server](databases/sqlserver.md)
- [Migrating from other tools](migrating/index.md): bring your connections from Beekeeper Studio, Azure Data Studio or pgAdmin, with a feature map for each

## Browse and write SQL

- [Object browser](object-browser.md): the lazy schema tree, context menus, DDL and the guarded DROP
- [DDL and scripts](ddl.md): generated CREATE, SELECT, INSERT and DROP statements
- [SQL editor](sql-editor.md): highlighting, multi-cursor, formatting and intellisense
- [Running queries](running-queries.md): run a statement, a selection or a script, cancel, parameters, Explain and the production confirmation
- [Query history and saved queries](query-history.md)

## Work with results

- [Results grid](results-grid.md): columns, selection, cell detail, sort, filter and pages
- [Export and copy](export.md): copy the selection, export to CSV, JSON, SQL, Markdown, Excel or Parquet
- [Inline editing](inline-editing.md): stage cell edits, preview the SQL and apply in one transaction

## AI and extensions

- [AI configuration](ai.md): providers, the AI view, editor actions, AI sessions, agent tabs and the audit log
- [Plugins](plugins.md): the plugin host, the Plugins view and permissions

## Command line

- [Command-line interface](cli.md): run a statement, export, and start the MCP server from a terminal
