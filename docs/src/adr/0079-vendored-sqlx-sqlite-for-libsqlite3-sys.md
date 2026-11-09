# ADR-0079: Vendored sqlx-sqlite for libsqlite3-sys 0.39

- Status: accepted
- Date: 2026-11-09
- Deciders: SQLer maintainers

## Context and Problem Statement

`cargo deny check` fails on RUSTSEC-2026-0459, an advisory in the SQLite that `libsqlite3-sys` bundles
in versions before 0.39.0. `Cargo.lock` had 0.37.0, pulled by `sqlx-sqlite` 0.9.0 through the
`sqlite-bundled` feature of `sqlx`.

Version requirements found:

| Crate | Requirement on `libsqlite3-sys` |
| ----- | ------------------------------- |
| `sqlx-sqlite` 0.9.0 (latest published) | `>=0.30.1, <0.38.0` |
| `sqlx` 0.9.0 | `^0.37.0`, as a dev-dependency only (not resolved for a dependency) |

So `cargo update -p libsqlite3-sys` cannot leave 0.37, and no published `sqlx-sqlite` allows 0.39.1 or
newer. Only one `libsqlite3-sys` may exist in the graph (`links = "sqlite3"`).

## Decision Drivers

- No advisory ignore.
- No waiting for an upstream release.
- The smallest diff, with a clear drop condition.

## Considered Options

- Wait for an upstream `sqlx-sqlite` release.
- Ignore the advisory in `deny.toml`.
- A git `[patch]` to an upstream sqlx branch.
- Drop the `sqlite-bundled` build and link the system SQLite.
- Vendor `sqlx-sqlite` 0.9.0 with a relaxed `libsqlite3-sys` range.

## Decision Outcome

Chosen option: vendor `sqlx-sqlite` 0.9.0 under `vendor/sqlx-sqlite` with the requirement changed to
`>=0.39.1, <0.40.0`, wired through `[patch.crates-io]` like `vendor/sqlx-mysql` (ADR-0018). `sqlx` and
`sqlx-macros-core` ask for `sqlx-sqlite = "=0.9.0"`, so the patch replaces the crate everywhere and no
new workspace pin is needed. The copy compiles against `libsqlite3-sys` 0.39.1 without a source change.

### Consequences

- A second vendored crate to re-sync on every `sqlx` bump; the README and the drop condition (a
  published `sqlx-sqlite` allowing `libsqlite3-sys >= 0.39.1`) are in `vendor/sqlx-sqlite/README.md`.
- The newer bundled SQLite is compiled in; the `sqler-driver-sqlite` and `sqler-config` suites cover it.
- `deny.toml` does not change.

### Confirmation

- `cargo tree --workspace -i libsqlite3-sys` shows one version, 0.39.1, reached through `sqlx-sqlite`.
- `cargo tree --workspace -i sqlx-sqlite` shows the `vendor/sqlx-sqlite` path.
- `cargo deny fetch && cargo deny check` passes with no ignore for RUSTSEC-2026-0459.

## Pros and Cons of the Options

- Wait for upstream: rejected, security findings are fixed in the change that finds them (AGENTS.md §4).
- `deny.toml` ignore: rejected, it silences a real vulnerability.
- Git patch to an upstream branch: rejected, no such branch exists, and `unknown-git = "deny"` would need
  a `sources` entry.
- System SQLite: rejected, it does not fix the Windows and macOS CI builds and makes SQLer depend on the
  SQLite of the host.
- Vendor with a relaxed range: chosen, a one-line diff that is dropped once upstream catches up.

## More Information

- ADR-0018: precedent for a vendored sqlx crate under `[patch.crates-io]`.
