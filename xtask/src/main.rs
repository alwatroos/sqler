// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Repository automation tasks (generate WIT bindings, release, bench, plugin build).
//!
//! Usage: `cargo run -p xtask -- plugins <build|check|list|clean>` and
//! `cargo run -p xtask -- release <manifest|verify>`. Exit codes: 0 ok, 1 failure, 2 usage error.

#![forbid(unsafe_code)]

mod plugins;
mod release;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some((task, rest)) = args.split_first() {
        if task == "plugins" {
            return plugins::run(rest);
        }
        if task == "release" {
            return release::run(rest);
        }
    }
    eprintln!("error: expected a task, one of: plugins, release");
    eprintln!(
        "usage: cargo run -p xtask -- plugins <build [--release] [<name>...]|check|list|clean>"
    );
    eprintln!("       cargo run -p xtask -- release <manifest|verify> ...");
    ExitCode::from(2)
}
