// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Repository automation tasks (generate WIT bindings, release, bench, plugin build).
//!
//! Usage: `cargo run -p xtask -- plugins <build|check|list|clean>`,
//! `cargo run -p xtask -- release <manifest|verify>` or `cargo run -p xtask -- bench [options]` or `cargo run -p xtask -- icons`.
//! Exit codes: 0 ok, 1 failure (for `bench`: a budget breach or a failed measurement), 2 usage error.

#![forbid(unsafe_code)]

mod bench;
mod icons;
mod plugins;
mod release;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some((task, rest)) = args.split_first() {
        match task.as_str() {
            "plugins" => return plugins::run(rest),
            "release" => return release::run(rest),
            "bench" => return bench::run(rest),
            "icons" => return icons::run(rest),
            _ => {}
        }
    }
    eprintln!("error: expected a task, one of: plugins, release, bench, icons");
    eprintln!(
        "usage: cargo run -p xtask -- plugins <build [--release] [<name>...]|check|list|clean>"
    );
    eprintln!("       cargo run -p xtask -- release <manifest|verify> ...");
    eprintln!("       cargo run -p xtask -- icons");
    eprintln!("       {}", bench::usage().trim_start_matches("usage: "));
    ExitCode::from(2)
}
