# ADR-0069: Binary size budget raised from 60 MB to 70 MB

- Status: accepted
- Date: 2026-10-23
- Deciders: product owner (decision of 2026-10-23), recorded by the SQLer maintainers

## Context and Problem Statement

The release binary of `sqler` had a budget of 60 MB (62,914,560 bytes; prompt section 2.2, AGENTS.md section 1,
[ADR-0001](0001-ui-framework-spike-plan.md)). Linux is the tightest OS. At the M5 close the Linux release binary was
62,512,360 bytes, and 62,521,192 bytes on the `main` that carries the plugin work, which leaves about 0.4 MB of
headroom. The plugin host (wasmtime with Cranelift costs about +14.5 MB on Linux, the Pulley runtime about +3.6 MB,
[ADR-0054](0054-plugin-runtime-and-driver-feasibility.md)) and the AI stack keep adding code, and the next feature of any
size would cross the budget. `size.yml` is report only, so nothing stops a crossing; it only warns.

Should the budget stay at 60 MB and force a trim now, or be raised?

## Decision

The owner decided to raise the budget to **70 MB = 73,400,320 bytes** (70 x 1,048,576). Everything that carried the
60 MB figure now carries the new one:

- `AGENTS.md` (section 1 quality bar, section 3 `size.yml` line, the plugins line, section 9 status).
- `.github/workflows/size.yml`: `budget=73400320`; the warning when the default binary is over the budget and the
  `plugins` limit (budget minus a 1 MiB reserve, now 72,351,744 bytes) follow from it. `size.yml` stays report only,
  as before, and still prints the per-OS bytes and MiB of the default and the `plugins` binary on every run.
- `docs/src/benchmarks.md`: the target table and the method text.
- Older ADRs get a dated "Superseded in part by" line under their status; their text is not rewritten.

There is no size threshold in `xtask` or in another test; the only machine-read value is the one in `size.yml`.

## Consequences

- The Linux release binary has about 10.9 MB (10.4 MiB) of headroom (73,400,320 - 62,521,192 = 10,879,128 bytes) instead of 0.4 MB.
- The plugin host stays **off by default** regardless ([ADR-0060](0060-plugins-in-the-workbench.md)): the `plugins`
  feature is on by default only if the binary with it stays below the budget minus 1 MiB on every OS, and the measured
  Cranelift cost (+14.5 MB) does not fit that either. This ADR does not change the default.
- The per-OS release numbers are still reported by `size.yml`; macOS and Windows stay far below the budget.
- Trimming the binary (`opt-level = "s"` for non-hot dependencies, `zbus`/`accesskit_unix`, the `wgpu`/`naga`
  backends) is not done here and stays an option when the new budget tightens.
- The lightweight goals other than size (cold start < 500 ms, idle RSS < 150 MB with three connections) are unchanged.

## Amendment 1 (2026-10-23): budget 80 MB

The owner raised the budget again (owner decision 12 of the 1.0 plan, on top of the decision above), to
**80 MB = 83,886,080 bytes** (80 x 1,048,576). The text above records the 70 MB state and is not rewritten.

- Why: the 1.0 release artifact ships the plugin host (`plugins`) and SQL Server
  ([ADR-0071](0071-sql-server-plugin.md)). With the host the Linux binary was 79,908,200 bytes at the M6 close, which is
  over 70 MB and cannot be trimmed enough in one release without dropping features.
- New figures: budget 83,886,080 bytes; the `plugins` limit (budget minus the 1 MiB reserve) is **82,837,504 bytes**.
- Everything that carried the 70 MB figure now carries the new one: `AGENTS.md` (sections 1, 3, 9, the plugin size
  rule), `.github/workflows/size.yml` (`budget=83886080`, still report only, still printing the default and the
  `plugins` build per OS), `docs/src/benchmarks.md` (target row, method text), the DuckDB size paragraph of
  `docs/src/plugin-development/drivers.md` (the DuckDB build is still over the budget, by 11,294,144 bytes) and the
  dated status lines of ADR-0001, 0009, 0029, 0032, 0054, 0057 and 0060.
- `default` features are not flipped by this amendment: the plugin host and SQL Server in the release artifact are the
  business of the release build item of the 1.0 plan, which is measured against this budget.
- Still no size threshold in `xtask` or another test; the only machine-read value is `budget` in `size.yml`.
