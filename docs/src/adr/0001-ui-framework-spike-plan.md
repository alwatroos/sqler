# ADR-0001: UI framework spike plan

- Status: proposed
- Date: 2026-10-20
- Deciders: SQLer maintainers

## Context and Problem Statement

SQLer needs a native, GPU-accelerated Rust UI with a results grid that handles very large result
sets and a SQL editor with syntax highlighting, on Linux, macOS and Windows. The framework choice
is hard to reverse, so it is validated by a spike before the UI is built. This ADR is the spike
plan; the spike itself is M1.

## Decision Drivers

- Rust everywhere; no web view as the primary UI (AGENTS.md section 1, non-negotiables).
- Lightweight: cold start < 500 ms, idle RSS < 150 MB, release binary < 60 MB.
- A virtualised grid for 1M rows and a highlighted editor must work on all three OSes.
- Component availability, to avoid building a widget library from scratch.

## Considered Options

Versions from the 2026-10-20 survey in dependencies.md; re-verify at spike start.

- GPUI + gpui-component (default). Which crates.io distribution, `gpui` 0.2.2 or the `gpui-ce` fork
  0.2.2, is decided by the spike.
- iced 0.14.0 (fallback).
- Tauri / Dioxus-webview.

## Decision Outcome

Run a one-week spike in M1 with GPUI + gpui-component as the default candidate.

Spike plan:

- Build (a) a results grid with 1M virtualised rows and (b) a SQL editor with syntax highlighting.
- Run both on Linux (X11 and Wayland), macOS and Windows.

Pass criteria: both work on all three OSes. Failure on any target OS means iced is used instead.

Cold start, RSS and binary size are recorded against the targets in [Benchmarks](../benchmarks.md)
as data. They are not pass/fail criteria.

Spike code is not merged into `crates/`.

### Consequences

- The UI crate (`sqler-ui`) starts only after the spike outcome is recorded.
- If GPUI fails, a new ADR records iced and supersedes this one.

### Confirmation

The M1 outcome sets the Status to accepted, with the results under More Information.

## Pros and Cons of the Options

### GPUI + gpui-component

- Good: GPU-accelerated; ships a data table, a rope-based text editor and resizable panels.
- Bad: young ecosystem; distribution on crates.io must be chosen; platform maturity is unproven, hence the spike.

### iced

- Good: pure Rust, Elm-style, cross-platform.
- Bad: fewer ready-made components for a grid and an editor.

### Tauri / Dioxus-webview

- Rejected: a web view as the primary UI violates the non-negotiable in AGENTS.md section 1 (Rust everywhere, lightweight).

## More Information

- [Dependencies](../dependencies.md)
- Spike results: planned (M1).
