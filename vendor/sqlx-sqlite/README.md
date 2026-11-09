# Vendored sqlx-sqlite 0.9.0 (SQLer patch)

This is a copy of [`sqlx-sqlite` 0.9.0](https://crates.io/crates/sqlx-sqlite/0.9.0)
(upstream repository `launchbadge/sqlx`, commit `003b698e99e024f3621b8043a2426fde5b741171`,
path `sqlx-sqlite`). It is wired in through `[patch.crates-io]` in the root `Cargo.toml` and is
**not** a workspace member, so `cargo fmt`, `cargo clippy` and `.github/scripts/check-spdx.sh`
do not touch it. Upstream keeps its own licences: `LICENSE-APACHE` and `LICENSE-MIT`.

## Why

The bundled SQLite of `libsqlite3-sys` older than 0.39.0 is affected by RUSTSEC-2026-0459, and
`cargo deny check` fails on it. `sqlx-sqlite` 0.9.0 caps the dependency at `<0.38.0`, so
`cargo update` cannot reach a fixed version and no published `sqlx-sqlite` allows one (ADR-0079).

## The diff against upstream 0.9.0 (1 changed line, 1 file)

- `Cargo.toml` (1 line): in `[dependencies.libsqlite3-sys]` the `version` changes from
  `">=0.30.1, <0.38.0"` to `">=0.39.1, <0.40.0"`. The features `pkg-config` and `vcpkg` stay.

The crate compiled against `libsqlite3-sys` 0.39.1 without any source change.

Nothing else differs.

`Cargo.toml.orig` is the unmodified upstream manifest; `Cargo.lock` was not kept.

## Maintenance

- The workspace resolves `sqlx` 0.9.0, which asks for `sqlx-sqlite = "=0.9.0"`. On every sqlx bump:
  re-check whether the new `sqlx-sqlite` allows `libsqlite3-sys >= 0.39.1`; if not, download it from
  crates.io, re-apply the line above and update this file.
- Drop this directory and the `[patch.crates-io]` entry as soon as a published `sqlx-sqlite` allows
  `libsqlite3-sys >= 0.39.1`.
- Do not add other changes here.
