# Licences and SBOM

SQLer is licensed under the Apache License, Version 2.0 (`LICENSE`). The 1.0 release builds `sqler-app` with the
features `plugins` and `driver-mssql` (the `features` line of `dist-workspace.toml`, [ADR-0064](adr/0064-release-pipeline.md)).
This page records what third-party code that build contains, which duties the licences bring and how each one is met.

## What ships

Every archive (`.tar.xz` on macOS and Linux, `.zip` on Windows) holds, next to the `sqler` binary:

| File | Content |
| ---- | ------- |
| `LICENSE` | Apache-2.0 text of SQLer |
| `NOTICE` | SQLer notice and the attributions the licences ask for |
| `THIRD-PARTY-LICENSES.txt` | generated: every shipped crate with its licence text, upstream `NOTICE` files, licence files of the bundled C code |
| `licenses/LUCIDE-ISC.txt` | Lucide (ISC) and Feather (MIT) texts for the embedded icons |

The MSI installs the same four files next to its `bin` folder. The shell and PowerShell installers install the binary
only; they download the archive, so the bundle is in the archive they fetch.

`THIRD-PARTY-LICENSES.txt` is not committed. The `plan` job of `release.yml` generates it once and every build job
downloads it, so all targets ship the same bytes. To generate it locally (needs `cargo-about` 0.9.2 with its `cli`
feature, `jq` and the Rust toolchain):

```sh
sh .github/scripts/third-party-licenses.sh THIRD-PARTY-LICENSES.txt
```

The script reads the `features` and `targets` lines of `dist-workspace.toml`, runs `cargo about` with `about.toml` (the
accepted licences mirror `deny.toml`) and `about.hbs`, appends the `NOTICE` files of every shipped crate and the verbatim
licence files of `aws-lc-sys` and `ring`. The report covers the union of the
five release targets; a crate that only some targets build is listed once.

## Audit of the release set

Checked on 2026-10-24 with `cargo tree -p sqler-app --locked -e normal --features plugins,driver-mssql --target <triple>`
for the five targets, 816 distinct packages in the union (the workspace crates included, so the report lists fewer; a crate with two licences counts under the one chosen; 637 on aarch64 macOS, 640 on
x86_64 macOS, 626 on Windows, 732 on x86_64 Linux, 729 on aarch64 Linux).

| Licence (SPDX expression, summed) | Crates | Targets | Duty | How met |
| --------------------------------- | ------ | ------- | ---- | ------- |
| MIT (and `MIT OR ...` pairs where MIT is chosen) | 176 in the report | all | keep the copyright notice and licence text | `THIRD-PARTY-LICENSES.txt` (cargo-about picks one permitted alternative per crate) |
| Apache-2.0 (alone, or as the chosen side of an `OR`) | 620 in the report | all | licence text; keep upstream `NOTICE` | `THIRD-PARTY-LICENSES.txt`; the one `NOTICE` file in the tree (`parquet` 60.0.0, Apache Arrow) is reproduced in it and in `NOTICE` |
| Apache-2.0 WITH LLVM-exception (27 crates, among them `wasmtime` 50 and the `wasmtime-*` and `cranelift-*` crates) | 27 | all (plugin host) | licence text; the exception only relaxes the binary-notice duty | `THIRD-PARTY-LICENSES.txt` (listed under Apache-2.0). No `wasmtime` or `cranelift-*` crate ships a `NOTICE` file |
| ISC, BSD-3-Clause, BSD-2-Clause, Unicode-3.0, Zlib, 0BSD, CC0-1.0 | 23 ISC, 13 BSD-3-Clause, 1 BSD-2-Clause, 20 Unicode, 3 Zlib, 2 0BSD, 3 CC0 | all | notice and licence text (CC0: none) | `THIRD-PARTY-LICENSES.txt` |
| MPL-2.0 | `option-ext` in the tree of every target; `dwrote` only in `--target all` | `option-ext` all; `dwrote` none of the five | MPL-2.0 file-level copyleft: keep the licence text and make the source of MPL files available; both are used unmodified | MPL-2.0 text in `THIRD-PARTY-LICENSES.txt`; the source is on crates.io. Allowed per crate in `deny.toml` and `about.toml`, never globally |
| `Apache-2.0 OR GPL-2.0-only` (`self_cell`) | 1 | all | none under the Apache-2.0 alternative | the Apache-2.0 alternative is used |

Special trees:

- **wasmtime and cranelift** (feature `plugins`): 14 `wasmtime*` crates (the engine included) and 9 `cranelift*` crates. Apache-2.0 WITH LLVM-exception, no NOTICE files, no bundled C that needs a text of its own.
- **tiberius** 0.13.0 (feature `driver-mssql`): `MIT/Apache-2.0`, with `tiberius-macros`. TLS is `rustls` 0.23.46 (`Apache-2.0 OR ISC OR MIT`) on `ring` 0.17.14; `rustls-webpki` is ISC. One TLS stack (ADR-0018, ADR-0071).
- **ring** 0.17.14: `Apache-2.0 AND ISC`; it bundles BoringSSL-derived C and assembly. Its `LICENSE`, `LICENSE-BoringSSL` and `LICENSE-other-bits` are reproduced verbatim in `THIRD-PARTY-LICENSES.txt`. The terms need no acknowledgement line.
- **aws-lc-rs** 1.18.2 and **aws-lc-sys** 0.46.0 (bundled AWS-LC C code, also used by the patched `sqlx-mysql`): a conjunction of ISC, Apache-2.0, MIT and BSD-3-Clause parts. Its `LICENSE` is reproduced verbatim. It names OpenSSL and SSLeay code, but states that this code is under Apache-2.0, so there is no advertising clause and no acknowledgement is required.
- **cbindgen** (MPL-2.0) is a build-only tool of the GPUI tree. It is not in the dependency tree of the binary and is not shipped (`deny.toml` keeps its exception because `cargo deny` checks build dependencies).
- **dwrote** (MPL-2.0, DirectWrite bindings) is a Windows font crate. `cargo tree --target x86_64-pc-windows-msvc` does not list it; cargo-about does, so the report over-lists it, which is harmless.
- **Lucide and Feather** icons (ISC and MIT) are embedded as assets, not as crates; their texts are in `licenses/LUCIDE-ISC.txt`, shipped in every archive and the MSI.
- The `rsa` crate (RUSTSEC-2023-0071) is absent: `cargo tree -p sqler-app --locked -e normal --features plugins,driver-mssql --target all -i rsa` matches no package.

`cargo deny fetch` followed by `cargo deny check` passes (advisories, bans, licenses, sources) without a new exception or
advisory ignore. The existing advisory ignores of `deny.toml` are the GPUI ones recorded there; R14 adds none.

## SBOM

The `global artifacts` job writes one CycloneDX file per target, `sqler-app_<triple>.cdx.xml`, with the release features
(`cargo cyclonedx --target <triple> --target-in-filename`). The Windows `.zip` and `.msi` share the file of
`x86_64-pc-windows-msvc`.

## Not in 1.0

`DuckDB` (the `driver-duckdb` feature) is in no release artifact, so none of its licences are listed. The release guard
and the generating script fail if it ever enters the feature list or the output.
