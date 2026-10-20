# Introduction

SQLer is a lightweight, native, open-source (Apache-2.0) SQL IDE written in Rust. It is not a
general code editor with a database plugin: the database is the centre of the UI. Connections and
schema objects are first-class, and SQL editing is one activity among several.

Status: M0 bootstrap. The workspace, this book and the first architecture decisions exist; the
application itself is planned (M1 onwards).

## Where to go next

- [User guide](user-guide/index.md): installing and using SQLer.
- [Architecture](architecture/index.md): how the workspace is layered.
- [Architecture decisions](adr/index.md): the ADR log.
- [Plugin development](plugin-development/index.md): writing WebAssembly plugins.
- [AI](ai/index.md): providers, the tool contract and the safety model.
- [Benchmarks](benchmarks.md): tracked performance numbers.
- [Dependencies](dependencies.md): dependency choices and the version policy.
- [Contributing](contributing.md): building the book and the ADR rule.
