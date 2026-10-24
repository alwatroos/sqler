# Introduction

SQLer is a lightweight, native, open-source (Apache-2.0) SQL IDE written in Rust. It is not a
general code editor with a database plugin: the database is the centre of the UI. Connections and
schema objects are first-class, and SQL editing is one activity among several.

## What SQLer 1.0 is

- A desktop app with an object browser, a SQL editor, a results grid with export and inline editing, and a
  command palette.
- A command-line tool, `sqler-cli`, for running queries and exporting results without the window.
- SQLer 1.0 connects to: SQLite, PostgreSQL, MySQL/MariaDB, SQL Server.
- A plugin host in every download, with a built-in SQL Server plugin. Plugins ask for the capabilities they need and
  you approve them.
- AI features that are opt-in: they stay off until you configure a provider and turn them on for a connection.

Some things are not in 1.0; see [Not in 1.0](user-guide/connections.md#not-in-10).

## Where to go next

- [User guide](user-guide/index.md): installing and using SQLer.
- [Architecture](architecture/index.md): how the workspace is layered.
- [Architecture decisions](adr/index.md): the ADR log.
- [Plugin development](plugin-development/index.md): writing WebAssembly plugins.
- [AI](ai/index.md): providers, the tool contract and the safety model.
- [Benchmarks](benchmarks.md): tracked performance numbers.
- [Dependencies](dependencies.md): dependency choices and the version policy.
- [Contributing](contributing.md): building the book and the ADR rule.
