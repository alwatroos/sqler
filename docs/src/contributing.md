# Contributing

The project handbook is `AGENTS.md` at the repository root. It covers architecture, commands,
coding standards, the Definition of Done and the iteration workflow. Read it first.

## Building the book

The book is built with mdBook 0.5.4 (latest stable on crates.io, checked 2026-10-20).

```text
cargo install mdbook --version 0.5.4 --locked
mdbook build docs
mdbook serve docs --open
```

The output is `target/book/`. A new page needs a line in `docs/src/SUMMARY.md`; the build fails
when SUMMARY links a file that does not exist.

## Architecture decisions

An ADR comes before code for framework, plugin ABI, AI tool contract and storage format decisions.
See [ADR-0000](adr/0000-adr-process.md).
