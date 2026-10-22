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

## CI

The rules are in [ADR-0068](adr/0068-ci-pipeline-speed.md). In short:

- On a pull request, a `changes` job looks at the files you touched. Docs-only changes skip the Rust, database, plugin
  and screenshot work; every check name still reports (skipped counts as passed), and the `CI result` check fails if
  any CI job failed.
- On a pull request, the Windows job builds everything but runs tests only for the crates with a Windows-specific
  history. `main` runs the full matrix on all three OSes.
- To force the full matrix on a pull request, add the label `full-ci` and push again (the label is read when a run
  starts), or run `gh workflow run ci.yml --ref <your-branch>`.
- Caches are saved by runs on `main` only. A PR restores them; a PR that changes `Cargo.lock` or the toolchain starts
  cold and is slower.
- A new push to a PR branch cancels the running CI for that PR. Runs on `main` are never cancelled.
- Timings of recent runs: `sh .github/scripts/ci-timings.sh ci.yml 10 main` (needs the `gh` CLI; read-only).

## Architecture decisions

An ADR comes before code for framework, plugin ABI, AI tool contract and storage format decisions.
See [ADR-0000](adr/0000-adr-process.md).
