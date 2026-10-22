// SPDX-License-Identifier: Apache-2.0
//! `plugin-spike-host`: a throwaway wasmtime host for the plugin-runtime spike (ADR-0054).
//!
//! One cargo feature picks the engine (see `variant.rs`); `base` has none, so
//! `variant bytes - base bytes` is the measured size delta. Output is `SPIKE key=value`
//! on stdout. Exit codes: 0 ok, 1 failure, 2 usage error.

mod cli;
mod report;
#[cfg(feature = "engine")]
mod rt;
#[cfg(not(feature = "engine"))]
#[path = "rt_none.rs"]
mod rt;
mod variant;

use std::process::ExitCode;

use cli::Command;

/// Resident set size of this process in KiB (`memory-stats`), if the platform reports it.
#[must_use]
pub fn rss_kib() -> Option<u64> {
    memory_stats::memory_stats().map(|m| m.physical_mem as u64 / 1024)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    match run(command) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let first = e.lines().next().unwrap_or("failed");
            report::emit("error", first);
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(command: Command) -> Result<u8, String> {
    match command {
        Command::Info => {
            report::emit("variant", variant::NAME);
            report::emit("wasmtime_version", variant::WASMTIME_VERSION);
            report::emit("features", variant::wasmtime_features());
            Ok(0)
        }
        Command::RunFixture(path) => rt::run_fixture(&path).map(|()| 0),
        Command::Bench {
            path,
            instances,
            samples,
            calls,
        } => rt::bench(&path, instances, samples, calls).map(|()| 0),
        Command::Precompile {
            input,
            output,
            pulley,
        } => rt::precompile(&input, &output, pulley).map(|()| 0),
        Command::RunCommand(rc) => rt::run_command(&rc),
    }
}
